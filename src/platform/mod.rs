use thiserror::Error;

#[cfg(target_os = "linux")]
pub mod x11;

/// 当前构建实际使用的后端。
///
/// 上层只通过这个名字使用平台能力；接入 Windows 时只需在此处按 `cfg`
/// 指向 `platform::win`，装配代码不用改。
#[cfg(target_os = "linux")]
pub use x11::{compositor_active, overlay, X11Platform as Platform};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RectI {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct MonitorInfo {
    pub physical: RectI,
    pub logical: RectF,
    pub scale: f64,
}

impl MonitorInfo {
    pub fn width_pt(&self) -> f64 {
        self.logical.width * 72.0 / 96.0
    }

    pub fn height_pt(&self) -> f64 {
        self.logical.height * 72.0 / 96.0
    }
}

#[derive(Debug, Clone)]
pub struct CaptureImage {
    pub width: u32,
    pub height: u32,
    pub origin_px: (i32, i32),
    pub rgba: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("X11 连接失败: {0}")]
    X11Connect(String),
    #[error("X11 请求失败: {0}")]
    X11Request(String),
    #[error("找不到主显示器")]
    NoMonitor,
    #[error("屏幕不支持透明窗口（缺少 RGBA visual）")]
    TransparencyUnsupported,
    #[error("窗口没有 X11 句柄")]
    NoX11Window,
    #[error("截图失败: {0}")]
    Capture(String),
    #[error("不支持的 X11 像素格式: {0}")]
    UnsupportedPixelFormat(String),
}
