//! 覆盖层窗口的命中测试状态与面板矩形发现。
//!
//! Windows 上不需要 X11 那套「XFixes 输入区域挖洞」，直接在 `WM_NCHITTEST` 里
//! 返回 `HTTRANSPARENT` 就能让点击落到下层窗口（包括面板）。

use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};

use crate::platform::RectI;

pub const HTTRANSPARENT: isize = -1;
pub const HTCLIENT: isize = 1;

/// 命中测试需要知道的全局状态（窗口过程是裸函数，只能靠静态变量取）。
static HIT_TEST: Mutex<HitTestState> = Mutex::new(HitTestState {
    passthrough: false,
    panel: None,
});

#[derive(Debug, Clone, Copy)]
struct HitTestState {
    passthrough: bool,
    /// 面板矩形（屏幕物理像素）。
    panel: Option<RectI>,
}

pub fn set_passthrough(enabled: bool) {
    if let Ok(mut state) = HIT_TEST.lock() {
        state.passthrough = enabled;
    }
}

pub fn set_panel_rect(rect: Option<RectI>) {
    if let Ok(mut state) = HIT_TEST.lock() {
        state.panel = rect;
    }
}

/// 穿透判定：开启穿透，或点击落在面板矩形内 → `HTTRANSPARENT`。
pub fn hit_test(screen_x: i32, screen_y: i32) -> isize {
    let Ok(state) = HIT_TEST.lock() else {
        return HTCLIENT;
    };
    if state.passthrough {
        return HTTRANSPARENT;
    }
    if let Some(panel) = state.panel {
        let inside = screen_x >= panel.x
            && screen_x < panel.x + panel.width as i32
            && screen_y >= panel.y
            && screen_y < panel.y + panel.height as i32;
        if inside {
            return HTTRANSPARENT;
        }
    }
    HTCLIENT
}

/// `lParam` 的低/高 16 位是两个有符号坐标（C 宏 `GET_X_LPARAM` / `GET_Y_LPARAM` 的等价物）。
pub fn lparam_point(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam as u16) as i16 as i32;
    let y = ((lparam >> 16) as u16) as i16 as i32;
    (x, y)
}

/// 枚举顶层窗口找面板（标题 `LInk`），返回屏幕物理像素矩形。
pub fn find_panel_rect() -> Option<RectI> {
    unsafe extern "system" fn callback(hwnd: HWND, data: isize) -> i32 {
        let out = data as *mut Option<RectI>;
        if (*out).is_some() {
            return 0; // 已找到，停止枚举
        }
        if is_canvas_window(hwnd) {
            return 1;
        }
        let mut title = [0u16; 64];
        let len = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW(
                hwnd,
                title.as_mut_ptr(),
                title.len() as i32,
            )
        };
        if len <= 0 {
            return 1;
        }
        if String::from_utf16_lossy(&title[..len as usize]).trim() != "LInk" {
            return 1;
        }
        let mut rect = RECT::default();
        let ok =
            unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect) };
        if ok == 0 {
            return 1;
        }
        let width = (rect.right - rect.left).max(0) as u32;
        let height = (rect.bottom - rect.top).max(0) as u32;
        if width > 0 && height > 0 {
            *out = Some(RectI {
                x: rect.left,
                y: rect.top,
                width,
                height,
            });
            return 0;
        }
        1
    }

    let mut out: Option<RectI> = None;
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(callback),
            &mut out as *mut Option<RectI> as isize,
        );
    }
    out
}

fn is_canvas_window(hwnd: HWND) -> bool {
    let mut class = [0u16; 64];
    let len = unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW(
            hwnd,
            class.as_mut_ptr(),
            class.len() as i32,
        )
    };
    if len <= 0 {
        return false;
    }
    String::from_utf16_lossy(&class[..len as usize]) == super::window::WINDOW_CLASS
}

/// 每 400 ms 刷新一次面板位置（面板可拖动，与 Linux 的定时器节奏一致）。
pub fn refresh_panel_rect() {
    set_panel_rect(find_panel_rect());
}
