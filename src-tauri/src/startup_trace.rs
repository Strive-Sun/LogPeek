use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

const TRACE_PATH_ENV: &str = "LOGCRATE_STARTUP_TRACE";
static PROCESS_START: OnceLock<Instant> = OnceLock::new();

pub fn record_process_start() {
    let _ = PROCESS_START.set(Instant::now());
}

#[derive(Clone)]
pub struct StartupTrace {
    started: Instant,
    stages: Arc<Mutex<Vec<StartupStage>>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupStage {
    name: String,
    elapsed_micros: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupSnapshot {
    process_id: u32,
    stages: Vec<StartupStage>,
}

impl Default for StartupTrace {
    fn default() -> Self {
        let started = *PROCESS_START.get_or_init(Instant::now);
        let trace = Self {
            started,
            stages: Arc::new(Mutex::new(Vec::new())),
        };
        trace.mark("process-start");
        trace
    }
}

impl StartupTrace {
    pub fn mark(&self, name: &str) {
        let elapsed_micros = self.started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        let mut stages = self
            .stages
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if stages.iter().any(|stage| stage.name == name) {
            return;
        }
        stages.push(StartupStage {
            name: name.to_owned(),
            elapsed_micros,
        });
        drop(stages);

        #[cfg(debug_assertions)]
        eprintln!("[startup] {name}: {:.1} ms", elapsed_micros as f64 / 1000.0);

        if name == "interactive-frame" {
            self.flush_async();
        }
    }

    fn snapshot(&self) -> StartupSnapshot {
        let stages = self
            .stages
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        StartupSnapshot {
            process_id: std::process::id(),
            stages,
        }
    }

    fn flush_async(&self) {
        let Some(path) = std::env::var_os(TRACE_PATH_ENV).map(PathBuf::from) else {
            return;
        };
        let trace = self.clone();
        std::thread::spawn(move || {
            // Give work released by the interactive gate a short opportunity to publish its
            // scheduling stages. This writer is benchmark-only and never blocks the UI thread.
            std::thread::sleep(std::time::Duration::from_millis(200));
            let snapshot = trace.snapshot();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(data) = serde_json::to_vec_pretty(&snapshot) {
                let _ = std::fs::write(path, data);
            }
        });
    }

    #[cfg(test)]
    fn stages(&self) -> Vec<StartupStage> {
        self.stages
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_stages_are_recorded_once_in_order() {
        let trace = StartupTrace::default();
        trace.mark("native-first-frame");
        trace.mark("native-first-frame");
        trace.mark("interactive-frame");
        let names = trace
            .stages()
            .into_iter()
            .map(|stage| stage.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec!["process-start", "native-first-frame", "interactive-frame"]
        );
    }
}
