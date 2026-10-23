// Windows release 模式下不弹控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    cfg_explorer_desktop_lib::run();
}
