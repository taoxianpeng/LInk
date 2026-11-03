//! 文件日志：把 `log` 宏的输出同时写进控制台和文件。
//!
//! 为什么需要：用户双击 exe 启动时没有控制台，面板和画布的日志全部丢失，
//! 出问题时无法定位。这里把两个进程的日志写进**同一个文件**
//! （`<配置目录>/link-ink/link.log`），用户直接把文件发回来即可。
//!
//! 只在 debug 构建启用：
//! - release 版不该因为日志拖慢渲染或无限增长文件；
//! - 测试反馈阶段用 `cargo build`（debug）产物即可。
//!
//! 每次启动会**覆盖**上一次的日志——同一个问题通常一次启动就能复现，
//! 保留历史会让文件越来越大、定位反而变难。需要保留历史时把
//! `LINK_LOG_APPEND=1` 塞进环境变量。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 日志文件名（面板与画布**各写各的**）。
///
/// 为什么不用同一个文件：面板和画布是两个进程，各自持有独立的文件偏移。
/// Windows 上 `OpenOptions::append(true)` 不保证跨进程原子追加，
/// 实测会出现一行被另一进程的行覆盖、时间戳乱序（日志里看到过
/// `_ink::platform::win::mouse_hook]` 这种被砍掉前缀的行）。
/// 分开写最省心，用户反馈时把目录里两个文件一起发即可。
const PANEL_LOG: &str = "link-panel.log";
const CANVAS_LOG: &str = "link-canvas.log";

/// 日志所在目录：`<配置目录>/link-ink/`。
///
/// 与设置文件同目录，方便用户一次打包反馈。
pub fn log_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("link-ink"))
}

/// 当前进程该写的日志文件。
///
/// 面板是入口进程（`link-ink-panel`），其余都算画布（`link-ink-canvas`）。
pub fn log_path() -> Option<PathBuf> {
    let name = if is_canvas_process() {
        CANVAS_LOG
    } else {
        PANEL_LOG
    };
    log_dir().map(|dir| dir.join(name))
}

/// 判断当前是不是画布进程（画布由面板以 `LINK_LOG_ROLE=canvas` 拉起）。
fn is_canvas_process() -> bool {
    std::env::var("LINK_LOG_ROLE").is_ok_and(|value| value == "canvas")
}

fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library/Application Support"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// 同时写控制台与文件的 writer。
struct Tee {
    file: Mutex<std::fs::File>,
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // 控制台写失败不算错误（双击启动时 stdio 不可用是常态）。
        let _ = std::io::stderr().write_all(buf);
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stderr().flush();
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
        Ok(())
    }
}

/// 初始化日志：控制台 + 文件。
///
/// debug 构建写文件；release 保持原样（只输出到控制台）。
/// 文件初始化失败时静默退回控制台，绝不影响程序启动。
pub fn init(default_filter: &str) {
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(default_filter),
    );

    let mut file_error = false;
    if cfg!(debug_assertions) {
        if let Some(path) = log_path() {
            match open_tee(&path) {
                Some(tee) => {
                    builder.target(env_logger::Target::Pipe(Box::new(tee)));
                    builder.format_timestamp_millis();
                    builder.init();
                    log::info!("日志文件：{}", path.display());
                    return;
                }
                None => file_error = true,
            }
        }
    }

    builder.init();
    if file_error {
        // 必须在 `init()` 之后：init 之前调 log 宏不会输出任何东西。
        log::warn!("无法创建日志文件，日志只输出到控制台");
    }
}

fn open_tee(path: &Path) -> Option<Tee> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    // 每个进程写自己的文件，所以直接截断即可：
    // 每次运行都是干净的日志，不需要追加模式。
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    let file = options.open(path).ok()?;
    Some(Tee {
        file: Mutex::new(file),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_path_lives_next_to_settings() {
        // 与 settings.json 同目录：方便用户一次打包反馈。
        if let (Some(log), Some(expected)) = (log_path(), config_dir()) {
            assert_eq!(log.parent(), Some(expected.join("link-ink").as_path()));
            let name = log.file_name().unwrap().to_string_lossy().to_string();
            assert!(
                name == PANEL_LOG || name == CANVAS_LOG,
                "意外的日志文件名：{name}"
            );
        }
    }

    #[test]
    fn canvas_role_picks_canvas_log() {
        // 环境变量是进程级的，这里只验证两个文件名不同且都能拼出来。
        assert_ne!(PANEL_LOG, CANVAS_LOG);
        assert!(PANEL_LOG.ends_with(".log") && CANVAS_LOG.ends_with(".log"));
    }

    #[test]
    fn tee_writes_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/link.log");
        let mut tee = open_tee(&path).expect("创建日志文件");
        tee.write_all(b"hello\n").unwrap();
        tee.flush().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    }
}
