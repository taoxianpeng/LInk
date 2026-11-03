//! 截图：`BitBlt` 抓主屏物理像素，转成 straight RGBA。
//!
//! Windows 上截图必须：
//! - 先设置 DPI 感知（否则 `GetDC(NULL)` 得到的是被系统缩放的逻辑尺寸）；
//! - 用 `SRCCOPY | CAPTUREBLT`，否则分层窗口不会被合成进去；
//! - 抓的是**主屏**矩形（`rcMonitor`），与 `MonitorInfo.physical` 完全一致。

use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
};

use crate::platform::{CaptureImage, PlatformError, RectI};

pub fn capture(rect: RectI) -> Result<CaptureImage, PlatformError> {
    if rect.width == 0 || rect.height == 0 {
        return Err(PlatformError::Capture("截图区域为空".to_string()));
    }
    let width = rect.width as i32;
    let height = rect.height as i32;

    let screen_dc = unsafe { GetDC(std::ptr::null_mut()) };
    if screen_dc.is_null() {
        return Err(PlatformError::Capture("GetDC(NULL) 失败".to_string()));
    }
    let memory_dc = unsafe { CreateCompatibleDC(screen_dc) };
    if memory_dc.is_null() {
        unsafe {
            ReleaseDC(std::ptr::null_mut(), screen_dc);
        }
        return Err(PlatformError::Capture(
            "CreateCompatibleDC 失败".to_string(),
        ));
    }
    let bitmap = unsafe { CreateCompatibleBitmap(screen_dc, width, height) };
    if bitmap.is_null() {
        unsafe {
            DeleteDC(memory_dc);
            ReleaseDC(std::ptr::null_mut(), screen_dc);
        }
        return Err(PlatformError::Capture(
            "CreateCompatibleBitmap 失败".to_string(),
        ));
    }
    let previous = unsafe { SelectObject(memory_dc, bitmap as _) };

    let mut copied = unsafe {
        BitBlt(
            memory_dc,
            0,
            0,
            width,
            height,
            screen_dc,
            rect.x,
            rect.y,
            SRCCOPY | CAPTUREBLT,
        )
    };
    if copied == 0 {
        // 部分远程会话/旧驱动不支持 CAPTUREBLT，退回普通 SRCCOPY。
        copied = unsafe {
            BitBlt(
                memory_dc,
                0,
                0,
                width,
                height,
                screen_dc,
                rect.x,
                rect.y,
                SRCCOPY,
            )
        };
    }

    let mut bgra = Vec::new();
    if copied != 0 {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [Default::default()],
        };
        bgra = vec![0u8; (width * height * 4) as usize];
        let mut info = info;
        let lines = unsafe {
            windows_sys::Win32::Graphics::Gdi::GetDIBits(
                memory_dc,
                bitmap,
                0,
                height as u32,
                bgra.as_mut_ptr() as *mut _,
                &mut info,
                DIB_RGB_COLORS,
            )
        };
        if lines == 0 {
            bgra.clear();
        }
    }

    unsafe {
        SelectObject(memory_dc, previous);
        DeleteObject(bitmap as _);
        DeleteDC(memory_dc);
        ReleaseDC(std::ptr::null_mut(), screen_dc);
    }

    if bgra.is_empty() {
        return Err(PlatformError::Capture("BitBlt/GetDIBits 失败".to_string()));
    }

    // BGRA → straight RGBA（屏幕像素不透明，alpha 固定 255）。
    let mut rgba = vec![0u8; bgra.len()];
    for (dst, src) in rgba.chunks_exact_mut(4).zip(bgra.chunks_exact(4)) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
        dst[3] = 255;
    }

    Ok(CaptureImage {
        width: rect.width,
        height: rect.height,
        origin_px: (rect.x, rect.y),
        rgba,
    })
}
