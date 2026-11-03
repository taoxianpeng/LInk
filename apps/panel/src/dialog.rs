//! 文件选择对话框。
//!
//! **绝不能在 UI 线程里调用阻塞式对话框**：v0.2 预览里点「保存工程」后窗口会被
//! 窗口管理器判定为 “not responding”，因为 `rfd` 的 portal 调用会一直阻塞
//! egui 的更新循环。这里改成：工作线程跑对话框，UI 每帧非阻塞轮询结果。

use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[derive(Clone, Debug)]
pub enum Request {
    OpenXopp,
    SaveXopp,
    SaveImage { extension: &'static str },
}

pub struct Picker {
    receiver: Receiver<Option<PathBuf>>,
}

impl Picker {
    /// 在工作线程里弹对话框；完成后会唤醒 egui 重绘。
    pub fn start(ctx: egui::Context, request: Request) -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = run(request);
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        Self { receiver }
    }

    /// `None` = 还没选完；`Some(None)` = 取消；`Some(Some(path))` = 选中。
    pub fn poll(&self) -> Option<Option<PathBuf>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(None),
        }
    }
}

fn run(request: Request) -> Option<PathBuf> {
    // Linux 上优先用 zenity/kdialog：它们本来就是独立进程，天然不阻塞，
    // 也不依赖桌面门户是否可用。都找不到时再退回 rfd（portal / Win32）。
    #[cfg(target_os = "linux")]
    {
        if let Some(path) = run_linux_dialog(&request) {
            return Some(path);
        }
    }
    run_rfd(request)
}

#[cfg(target_os = "linux")]
fn run_linux_dialog(request: &Request) -> Option<PathBuf> {
    let (title, filter_name, filter_pattern, save, default_name) = match request {
        Request::OpenXopp => ("打开 Xournal++ 工程", "Xournal++ 工程", "*.xopp", false, ""),
        Request::SaveXopp => (
            "保存 Xournal++ 工程",
            "Xournal++ 工程",
            "*.xopp",
            true,
            "未命名.xopp",
        ),
        Request::SaveImage { extension } => (
            "导出图片",
            "图片",
            if *extension == "png" {
                "*.png"
            } else {
                "*.jpg"
            },
            true,
            if *extension == "png" {
                "屏幕标注.png"
            } else {
                "屏幕标注.jpg"
            },
        ),
    };

    if which("zenity") {
        let mut command = Command::new("zenity");
        command.arg("--file-selection").arg("--title").arg(title);
        if save {
            command.arg("--save").arg("--confirm-overwrite");
            command.arg(format!("--filename={default_name}"));
        }
        command.arg(format!("--file-filter={filter_name} | {filter_pattern}"));
        if let Some(output) = spawn_and_read(&mut command) {
            return Some(output);
        }
        // 用户取消时 zenity 返回非 0，视为取消，不再退回其它后端。
        return None;
    }

    if which("kdialog") {
        let mut command = Command::new("kdialog");
        if save {
            command.arg("--getsavefilename").arg(default_name);
        } else {
            command.arg("--getopenfilename");
            command.arg(".");
        }
        command.arg(format!("{filter_name} ({filter_pattern})"));
        return spawn_and_read(&mut command);
    }

    None
}

#[cfg(target_os = "linux")]
fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn spawn_and_read(command: &mut Command) -> Option<PathBuf> {
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let path = text.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn run_rfd(request: Request) -> Option<PathBuf> {
    let dialog = rfd::FileDialog::new();
    match request {
        Request::OpenXopp => dialog.add_filter("Xournal++ 工程", &["xopp"]).pick_file(),
        Request::SaveXopp => {
            let path = dialog
                .add_filter("Xournal++ 工程", &["xopp"])
                .set_file_name("未命名.xopp")
                .save_file()?;
            Some(ensure_extension(path, "xopp"))
        }
        Request::SaveImage { extension } => {
            let name = format!("屏幕标注.{extension}");
            let path = dialog
                .add_filter(extension, &[extension])
                .set_file_name(name)
                .save_file()?;
            Some(ensure_extension(path, extension))
        }
    }
}

fn ensure_extension(path: PathBuf, extension: &str) -> PathBuf {
    if path.extension().is_some() {
        return path;
    }
    let mut path = path;
    path.set_extension(extension);
    path
}
