#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
fn main() -> windows_service::Result<()> {
    service::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("LogCrate Index Service 仅支持 Windows");
}

#[cfg(windows)]
mod service {
    use logcrate_lib::ntfs::ipc::{
        enumerate_mft_via_service, query_usn_via_service, read_usn_via_service, run_pipe_server,
        wake_pipe_server, SERVICE_NAME,
    };
    use std::ffi::OsString;
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use widestring::U16CString;
    use windows_service::define_windows_service;
    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{service_dispatcher, Result};
    use windows_service::{service_manager::ServiceManager, service_manager::ServiceManagerAccess};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
    use windows_sys::Win32::System::Services::SetServiceObjectSecurity;

    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;
    const SERVICE_DACL: &str = concat!(
        "D:",
        "(A;;CCLCSWRPWPDTLOCRRC;;;SY)",
        "(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)",
        "(A;;CCLCSWRPLOCRRC;;;IU)"
    );

    pub fn run() -> Result<()> {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        if args.iter().any(|arg| arg == "--install") {
            return install();
        }
        if args.iter().any(|arg| arg == "--uninstall") {
            return uninstall();
        }
        if let Some(index) = args.iter().position(|arg| arg == "--probe") {
            let volume = args
                .get(index + 1)
                .and_then(|value| value.chars().next())
                .unwrap_or('C');
            let started = std::time::Instant::now();
            let summary = enumerate_mft_via_service(volume, |_| Ok(())).map_err(service_error)?;
            println!(
                "MFT service probe: {summary:?}, elapsed={:?}",
                started.elapsed()
            );
            return Ok(());
        }
        if let Some(index) = args.iter().position(|arg| arg == "--probe-usn") {
            let volume = args
                .get(index + 1)
                .and_then(|value| value.chars().next())
                .unwrap_or('C');
            let info = query_usn_via_service(volume).map_err(service_error)?;
            let summary = read_usn_via_service(
                volume,
                info.next_usn,
                info.journal_id,
                info.next_usn,
                |_| Ok(()),
            )
            .map_err(service_error)?;
            println!("USN service probe: info={info:?}, replay={summary:?}");
            return Ok(());
        }
        if args.iter().any(|arg| arg == "--console" || arg == "--once") {
            let stop = AtomicBool::new(false);
            let once = args.iter().any(|arg| arg == "--once");
            run_pipe_server(&stop, once).map_err(service_error)?;
            return Ok(());
        }
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
    }

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_arguments: Vec<OsString>) {
        let _ = run_service();
    }

    fn run_service() -> Result<()> {
        let stop = Arc::new(AtomicBool::new(false));
        let handler_stop = Arc::clone(&stop);
        let event_handler = move |event| match event {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            ServiceControl::Stop => {
                handler_stop.store(true, Ordering::SeqCst);
                wake_pipe_server();
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        };
        let status = service_control_handler::register(SERVICE_NAME, event_handler)?;
        status.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        let result = run_pipe_server(&stop, false);
        status.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(if result.is_err() { 1 } else { 0 }),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;
        result.map_err(service_error)
    }

    fn service_error(error: anyhow::Error) -> windows_service::Error {
        windows_service::Error::Winapi(std::io::Error::new(std::io::ErrorKind::Other, error))
    }

    fn install() -> Result<()> {
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )?;
        let binary = std::env::current_exe().map_err(windows_service::Error::Winapi)?;
        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from("LogCrate Index Service"),
            service_type: SERVICE_TYPE,
            start_type: ServiceStartType::OnDemand,
            error_control: ServiceErrorControl::Normal,
            executable_path: binary,
            launch_arguments: vec![],
            dependencies: vec![],
            account_name: None,
            account_password: None,
        };
        let access = ServiceAccess::ALL_ACCESS
            // windows-service 0.7 does not expose the standard WRITE_DAC
            // access bit, but SetServiceObjectSecurity requires it.
            | ServiceAccess::from_bits_truncate(0x0004_0000);
        let service = match manager.open_service(SERVICE_NAME, access) {
            Ok(service) => {
                service.change_config(&info)?;
                service
            }
            Err(_) => manager.create_service(&info, access)?,
        };
        service.set_description(
            "为 LogCrate 只读枚举本机 NTFS MFT/USN 文件名元数据；不读取文件内容。",
        )?;
        set_service_dacl(&service)?;
        if service.query_status()?.current_state == ServiceState::Stopped {
            service.start::<&str>(&[])?;
        }
        Ok(())
    }

    fn uninstall() -> Result<()> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(
            SERVICE_NAME,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
        )?;
        if service.query_status()?.current_state != ServiceState::Stopped {
            let _ = service.stop();
            for _ in 0..50 {
                if service.query_status()?.current_state == ServiceState::Stopped {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        service.delete()?;
        Ok(())
    }

    fn set_service_dacl(service: &windows_service::service::Service) -> Result<()> {
        let sddl = U16CString::from_str(SERVICE_DACL).map_err(|error| {
            windows_service::Error::Winapi(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                error,
            ))
        })?;
        let mut descriptor = null_mut();
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        };
        if converted == 0 {
            return Err(windows_service::Error::Winapi(
                std::io::Error::last_os_error(),
            ));
        }
        let success = unsafe {
            SetServiceObjectSecurity(
                service.raw_handle() as *mut std::ffi::c_void,
                DACL_SECURITY_INFORMATION,
                descriptor,
            )
        };
        unsafe {
            LocalFree(descriptor);
        }
        if success == 0 {
            return Err(windows_service::Error::Winapi(
                std::io::Error::last_os_error(),
            ));
        }
        Ok(())
    }
}
