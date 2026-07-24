// 阻止 Windows release 构建弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    logcrate_lib::record_process_start();
    logcrate_lib::run()
}
