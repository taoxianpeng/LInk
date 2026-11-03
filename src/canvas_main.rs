//! 原生画布进程入口：按平台分发到 GTK（Linux/X11）或 Win32（Windows）实现。
//!
//! Cargo 的 `[[bin]]` 只能指定一个路径，所以这里做编译期分发；
//! 两边共用同一份 [`link_ink::canvas_app::CanvasApp`] 控制器与 IPC 协议。

#[cfg(target_os = "linux")]
#[path = "canvas/gtk_main.rs"]
mod imp;

#[cfg(target_os = "windows")]
#[path = "canvas/win_main.rs"]
mod imp;

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod imp {
    pub fn run() {
        eprintln!("LInk 画布目前只支持 Linux/X11 与 Windows");
        std::process::exit(1);
    }
}

fn main() {
    imp::run();
}
