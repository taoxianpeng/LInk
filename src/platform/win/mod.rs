//! Windows 后端：主显示器信息 + 截图入口。

pub mod capture;
pub mod input;
pub mod mouse_hook;
pub mod overlay;
pub mod window;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForMonitor, MDT_EFFECTIVE_DPI, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SetProcessDPIAware, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

use crate::platform::{CaptureImage, MonitorInfo, PlatformError, RectF, RectI};

pub struct WinPlatform {
    monitor: MonitorInfo,
}

impl WinPlatform {
    pub fn new() -> Result<Self, PlatformError> {
        enable_dpi_awareness();
        let primary = primary_monitor()?;
        let physical = monitor_rect(primary).unwrap_or_else(|error| {
            log::warn!("主屏查询失败，退回虚拟桌面尺寸: {error}");
            virtual_screen_rect()
        });
        let dpi = monitor_dpi(primary).unwrap_or(96);
        let mut scale = dpi as f64 / 96.0;
        if !scale.is_finite() || scale <= 0.0 {
            scale = 1.0;
        }

        let monitor = MonitorInfo {
            physical,
            // 逻辑坐标 = 物理 / scale。
            logical: RectF {
                x: physical.x as f64 / scale,
                y: physical.y as f64 / scale,
                width: physical.width as f64 / scale,
                height: physical.height as f64 / scale,
            },
            scale,
        };

        log::info!(
            "主屏: 物理 {}x{}+{}+{}，scale {:.3}（DPI {dpi}）",
            monitor.physical.width,
            monitor.physical.height,
            monitor.physical.x,
            monitor.physical.y,
            monitor.scale
        );

        Ok(Self { monitor })
    }

    pub fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    pub fn capture_primary(&self) -> Result<CaptureImage, PlatformError> {
        capture::capture(self.monitor.physical)
    }
}

/// 进程级 DPI 感知：必须在创建任何窗口之前调用。
fn enable_dpi_awareness() {
    let ok =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 };
    if !ok {
        unsafe {
            SetProcessDPIAware();
        }
        log::info!("DPI 感知退回系统级（PerMonitorV2 不可用）");
    }
}

/// 主显示器：用 `MonitorFromPoint(0,0, MONITOR_DEFAULTTOPRIMARY)` 直接拿，
/// 比枚举回调简单可靠（`EnumDisplayMonitors` 在某些会话里会返回“无效句柄”）。
fn primary_monitor() -> Result<HMONITOR, PlatformError> {
    let point = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    let monitor = unsafe {
        windows_sys::Win32::Graphics::Gdi::MonitorFromPoint(
            point,
            windows_sys::Win32::Graphics::Gdi::MONITOR_DEFAULTTOPRIMARY,
        )
    };
    if monitor.is_null() {
        // 兜底：枚举第一个显示器。
        return first_monitor();
    }
    Ok(monitor)
}

/// 枚举第一个可用显示器（`MonitorFromPoint` 都失败时才会走到这里）。
fn first_monitor() -> Result<HMONITOR, PlatformError> {
    unsafe extern "system" fn callback(
        monitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut windows_sys::Win32::Foundation::RECT,
        data: isize,
    ) -> i32 {
        let out = data as *mut Option<HMONITOR>;
        if (*out).is_none() {
            *out = Some(monitor);
        }
        0 // 取到第一个就停
    }

    let mut out: Option<HMONITOR> = None;
    let ok = unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(callback),
            &mut out as *mut Option<HMONITOR> as isize,
        )
    };
    if ok == 0 {
        return Err(PlatformError::NoMonitor);
    }
    out.ok_or(PlatformError::NoMonitor)
}

fn monitor_rect(monitor: HMONITOR) -> Result<RectI, PlatformError> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return Err(PlatformError::NoMonitor);
    }
    let width = (info.rcMonitor.right - info.rcMonitor.left).max(0) as u32;
    let height = (info.rcMonitor.bottom - info.rcMonitor.top).max(0) as u32;
    if width == 0 || height == 0 {
        return Err(PlatformError::NoMonitor);
    }
    Ok(RectI {
        x: info.rcMonitor.left,
        y: info.rcMonitor.top,
        width,
        height,
    })
}

fn virtual_screen_rect() -> RectI {
    unsafe {
        RectI {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1) as u32,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1) as u32,
        }
    }
}

fn monitor_dpi(monitor: HMONITOR) -> Option<u32> {
    if monitor.is_null() {
        return None;
    }
    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    let result = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    // S_OK == 0；失败时用 96 DPI（1.0 缩放）兜底。
    if result == 0 && dpi_x > 0 {
        Some(dpi_x)
    } else {
        None
    }
}

/// DWM 自 Vista 起默认开启，分层窗口的逐像素 alpha 依赖它。
pub fn compositor_active() -> bool {
    true
}

/// 窗口句柄有效性（给上层诊断用）。
pub fn is_valid_window(hwnd: HWND) -> bool {
    !hwnd.is_null()
}
