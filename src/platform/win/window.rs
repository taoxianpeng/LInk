//! 窗口相关的 Win32 细节：类注册、分层位图、脏矩形上传。

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowRect, LoadCursorW, RegisterClassExW,
    UpdateLayeredWindowIndirect, CS_HREDRAW, CS_VREDRAW, HMENU,
    ULW_ALPHA, UPDATELAYEREDWINDOWINFO, WNDCLASSEXW, WNDPROC, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::platform::{PlatformError, RectI};

pub const WINDOW_CLASS: &str = "LInkOverlayWindow";
pub const WINDOW_TITLE: &str = "LInk 标注层";

const AC_SRC_OVER: u8 = 0;
const AC_SRC_ALPHA: u8 = 1;

/// 把 Rust 字符串转成以 NUL 结尾的 UTF-16。
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn module_handle() -> *mut c_void {
    unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null()) as _ }
}

/// 注册窗口类。重复注册会返回 0，但只要 `CreateWindowExW` 能成功就没问题。
pub fn register_class(wndproc: WNDPROC) -> Result<(), PlatformError> {
    let class_name = wide(WINDOW_CLASS);
    let cursor = unsafe { LoadCursorW(std::ptr::null_mut(), 32512 as _) }; // IDC_ARROW
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: wndproc,
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: module_handle() as _,
        hIcon: std::ptr::null_mut(),
        hCursor: cursor,
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        // 类已存在时返回 0；用 GetLastError 区分“已存在”（1410）是真错误。
        let error = std::io::Error::last_os_error();
        let code = error.raw_os_error().unwrap_or(0);
        if code != 1410 {
            return Err(PlatformError::WindowCreate(format!(
                "RegisterClassExW 失败：{error}（class={WINDOW_CLASS}）"
            )));
        }
        log::debug!("窗口类已注册，复用");
    }
    Ok(())
}

/// 创建分层覆盖层窗口（不抢焦点、不进任务栏、永远置顶）。
pub fn create_overlay_window(rect: RectI) -> Result<HWND, PlatformError> {
    let class_name = wide(WINDOW_CLASS);
    let title = wide(WINDOW_TITLE);
    let ex_style = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            rect.x,
            rect.y,
            rect.width as i32,
            rect.height as i32,
            std::ptr::null_mut(),
            std::ptr::null_mut() as HMENU,
            module_handle() as _,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(PlatformError::WindowCreate(format!(
            "CreateWindowExW 失败：{}",
            std::io::Error::last_os_error()
        )));
    }
    // 这里**不** ShowWindow。
    //
    // 分层窗口必须在第一次显示之前就把位图内容提交好（`UpdateLayeredWindowIndirect`
    // 带位置参数）。如果先 ShowWindow 再提交，DWM 会缓存“空窗口”状态，
    // 之后所有更新都被静默忽略（实测：纯色不透明底图也刷不出来）。
    // 调用方负责：提交首帧 → ShowWindow → SetWindowPos(HWND_TOPMOST)。
    Ok(hwnd)
}

pub fn destroy_window(hwnd: HWND) {
    if !hwnd.is_null() {
        unsafe {
            DestroyWindow(hwnd);
        }
    }
}

pub fn default_wndproc(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 分层窗口的 32bpp top-down DIB + 内存 DC。
///
/// `UpdateLayeredWindowIndirect` 要求预乘 BGRA、且位图必须是 top-down（`biHeight` 为负）。
pub struct LayeredBitmap {
    hdc: HDC,
    hbitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    width: u32,
    height: u32,
}

impl LayeredBitmap {
    pub fn new(width: u32, height: u32) -> Result<Self, PlatformError> {
        if width == 0 || height == 0 {
            return Err(PlatformError::WindowCreate("位图尺寸无效".to_string()));
        }
        let screen_dc = unsafe { GetDC(std::ptr::null_mut()) };
        if screen_dc.is_null() {
            return Err(PlatformError::WindowCreate("GetDC(NULL) 失败".to_string()));
        }
        let hdc = unsafe { CreateCompatibleDC(screen_dc) };
        unsafe {
            ReleaseDC(std::ptr::null_mut(), screen_dc);
        }
        if hdc.is_null() {
            return Err(PlatformError::WindowCreate(
                "CreateCompatibleDC 失败".to_string(),
            ));
        }

        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // 负高度 = top-down：第 0 行就是屏幕最上面一行。
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: width * height * 4,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [Default::default()],
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let hbitmap = unsafe {
            CreateDIBSection(hdc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0)
        };
        if hbitmap.is_null() || bits.is_null() {
            unsafe {
                DeleteDC(hdc);
            }
            return Err(PlatformError::WindowCreate(
                "CreateDIBSection 失败".to_string(),
            ));
        }
        let previous = unsafe { SelectObject(hdc, hbitmap as _) };
        Ok(Self {
            hdc,
            hbitmap,
            previous,
            bits: bits as *mut u8,
            width,
            height,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// 把 tiny-skia 的预乘 RGBA 转成位图要求的预乘 BGRA。
    ///
    /// 只转换 `dirty` 覆盖的行；`None` 表示整张。
    pub fn upload_premultiplied_rgba(&mut self, rgba: &[u8], dirty: Option<DirtyRect>) {
        let expected = (self.width * self.height * 4) as usize;
        if rgba.len() < expected {
            log::warn!("位图数据不足：{} < {expected}", rgba.len());
            return;
        }
        let (x0, y0, x1, y1) = match dirty {
            Some(dirty) => (
                dirty.x.max(0) as u32,
                dirty.y.max(0) as u32,
                dirty.x1.clamp(0, self.width as i32) as u32,
                dirty.y1.clamp(0, self.height as i32) as u32,
            ),
            None => (0, 0, self.width, self.height),
        };
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(self.bits, expected) };
        for y in y0..y1 {
            let row = (y * self.width) as usize;
            for x in x0..x1 {
                let index = (row + x as usize) * 4;
                bytes[index] = rgba[index + 2]; // B
                bytes[index + 1] = rgba[index + 1]; // G
                bytes[index + 2] = rgba[index]; // R
                bytes[index + 3] = rgba[index + 3]; // A
            }
        }
    }

    /// 把位图（或其一块）提交给分层窗口。
    ///
    /// - `positioned == false`：首次显示，需要给全量参数（位置 + 尺寸 + 内容）。
    /// - `positioned == true`：窗口已经在屏幕上，只更新内容；按文档 `pptDst`/`psize`
    ///   应传 `NULL`，否则部分驱动会**静默失败**（返回成功但画面不刷新）。
    pub fn update_window(
        &self,
        hwnd: HWND,
        origin: (i32, i32),
        dirty: Option<DirtyRect>,
        positioned: bool,
    ) -> Result<(), PlatformError> {
        let point = POINT {
            x: origin.0,
            y: origin.1,
        };
        let size = SIZE {
            cx: self.width as i32,
            cy: self.height as i32,
        };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA,
        };
        let rect = dirty.map(|dirty| RECT {
            left: dirty.x,
            top: dirty.y,
            right: dirty.x1,
            bottom: dirty.y1,
        });
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            hdcDst: std::ptr::null_mut(),
            pptDst: if positioned {
                std::ptr::null()
            } else {
                &point
            },
            psize: if positioned {
                std::ptr::null()
            } else {
                &size
            },
            hdcSrc: self.hdc,
            pptSrc: &source,
            crKey: 0,
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: rect
                .as_ref()
                .map(|rect| rect as *const RECT)
                .unwrap_or(std::ptr::null()),
        };
        // DIB section 的内存写入必须经 GdiFlush 才对 GDI 可见，
        // 否则 UpdateLayeredWindowIndirect 可能取到旧的位图内容。
        unsafe {
            windows_sys::Win32::Graphics::Gdi::GdiFlush();
            // 让窗口区域标脏，UpdateLayeredWindowIndirect 才会真正重新合成。
            windows_sys::Win32::Graphics::Gdi::InvalidateRect(
                hwnd,
                rect.as_ref()
                    .map(|rect| rect as *const RECT)
                    .unwrap_or(std::ptr::null()),
                0,
            );
        }
        let ok = unsafe { UpdateLayeredWindowIndirect(hwnd, &info) };
        if ok == 0 {
            return Err(PlatformError::WindowCreate(format!(
                "UpdateLayeredWindowIndirect 失败：{}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }
}

impl Drop for LayeredBitmap {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_null() {
                SelectObject(self.hdc, self.previous);
            }
            if !self.hbitmap.is_null() {
                DeleteObject(self.hbitmap as _);
            }
            if !self.hdc.is_null() {
                DeleteDC(self.hdc);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: i32,
    pub y: i32,
    pub x1: i32,
    pub y1: i32,
}

impl DirtyRect {
    pub fn from_bounds(x0: f64, y0: f64, x1: f64, y1: f64, width: u32, height: u32) -> Option<Self> {
        let x = x0.floor().max(0.0) as i32;
        let y = y0.floor().max(0.0) as i32;
        let x1 = x1.ceil().min(width as f64) as i32;
        let y1 = y1.ceil().min(height as f64) as i32;
        if x1 <= x || y1 <= y {
            None
        } else {
            Some(Self { x, y, x1, y1 })
        }
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            x: self.x.min(other.x),
            y: self.y.min(other.y),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// 加上一圈内边距，避免抗锯齿边缘被裁掉。
    pub fn inflate(self, pad: i32, width: u32, height: u32) -> Self {
        Self {
            x: (self.x - pad).max(0),
            y: (self.y - pad).max(0),
            x1: (self.x1 + pad).min(width as i32),
            y1: (self.y1 + pad).min(height as i32),
        }
    }
}

/// 窗口的屏幕矩形（物理像素）。
pub fn window_rect(hwnd: HWND) -> RectI {
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(hwnd, &mut rect);
    }
    RectI {
        x: rect.left,
        y: rect.top,
        width: (rect.right - rect.left).max(0) as u32,
        height: (rect.bottom - rect.top).max(0) as u32,
    }
}
