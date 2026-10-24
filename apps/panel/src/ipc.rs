//! 与画布子进程的 IPC 客户端：spawn + 读线程 + 命令写入。

use std::io::BufReader;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use link_ipc::{read_event, write_command, Command, Event};

pub struct CanvasClient {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<Event>,
    pub exited_reason: Option<String>,
}

impl CanvasClient {
    /// 启动画布进程，并把事件读进通道（每来一条就请求重绘）。
    pub fn spawn(ctx: egui::Context) -> Result<Self, String> {
        let binary = locate_canvas_binary()?;
        let mut child = ProcessCommand::new(&binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("启动画布失败（{}）：{error}", binary.display()))?;

        let stdin = child.stdin.take().ok_or("画布 stdin 不可用")?;
        let stdout = child.stdout.take().ok_or("画布 stdout 不可用")?;
        let (tx, events) = mpsc::channel();

        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_event(&mut reader) {
                    Ok(Some(event)) => {
                        if tx.send(event).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                    Ok(None) => break,
                    Err(error) => {
                        log::warn!("解析画布事件失败: {error}");
                        break;
                    }
                }
            }
            ctx.request_repaint();
        });

        let mut client = Self {
            child,
            stdin,
            events,
            exited_reason: None,
        };
        client.send(Command::Hello {
            proto: link_ipc::PROTOCOL_VERSION,
        });
        log::info!("画布进程已启动: {}", binary.display());
        Ok(client)
    }

    pub fn send(&mut self, command: Command) {
        if let Err(error) = write_command(&mut self.stdin, &command) {
            self.exited_reason = Some(format!("发送命令失败：{error}"));
            log::error!("发送命令失败: {error}");
        }
    }

    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.exited_reason.is_none() {
                        self.exited_reason = Some("画布进程已退出".to_string());
                    }
                    break;
                }
            }
        }
        events
    }
}

impl Drop for CanvasClient {
    fn drop(&mut self) {
        let _ = write_command(&mut self.stdin, &Command::Quit);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 画布二进制的位置：优先环境变量，其次与面板同目录，最后是开发时的 target 目录。
fn locate_canvas_binary() -> Result<PathBuf, String> {
    let name = if cfg!(windows) {
        "link-ink-canvas.exe"
    } else {
        "link-ink-canvas"
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(path) = std::env::var("LINK_CANVAS_BIN") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
        }
    }
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    candidates.push(PathBuf::from(format!("../../target/{profile}/{name}")));
    candidates.push(PathBuf::from(format!("target/{profile}/{name}")));

    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
    }
    Err(format!(
        "找不到画布二进制，请先 `cargo build --bin link-ink-canvas`，或用 LINK_CANVAS_BIN 指定。已尝试：{:?}",
        candidates
    ))
}
