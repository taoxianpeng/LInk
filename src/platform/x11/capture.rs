use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as XprotoConnectionExt, ImageFormat, ImageOrder};
use x11rb::rust_connection::RustConnection;

use crate::platform::{CaptureImage, PlatformError, RectI};

pub fn capture(
    conn: &RustConnection,
    screen_num: usize,
    rect: RectI,
) -> Result<CaptureImage, PlatformError> {
    if rect.width == 0 || rect.height == 0 {
        return Err(PlatformError::Capture("截图区域为空".to_string()));
    }
    if rect.width > u16::MAX as u32 || rect.height > u16::MAX as u32 {
        return Err(PlatformError::Capture("截图区域超出 X11 限制".to_string()));
    }
    if rect.x < i16::MIN as i32
        || rect.x > i16::MAX as i32
        || rect.y < i16::MIN as i32
        || rect.y > i16::MAX as i32
    {
        return Err(PlatformError::Capture("截图原点超出 X11 限制".to_string()));
    }

    let setup = conn.setup();
    let screen = setup
        .roots
        .get(screen_num)
        .ok_or_else(|| PlatformError::Capture("无效的 X11 screen".to_string()))?;

    let reply = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            screen.root,
            rect.x as i16,
            rect.y as i16,
            rect.width as u16,
            rect.height as u16,
            !0,
        )
        .map_err(|err| PlatformError::Capture(err.to_string()))?
        .reply()
        .map_err(|err| PlatformError::Capture(err.to_string()))?;

    let format = setup
        .pixmap_formats
        .iter()
        .find(|format| format.depth == reply.depth)
        .ok_or_else(|| {
            PlatformError::UnsupportedPixelFormat(format!(
                "找不到 depth={} 的 pixmap format",
                reply.depth
            ))
        })?;

    if format.bits_per_pixel != 32 || setup.image_byte_order != ImageOrder::LSB_FIRST {
        return Err(PlatformError::UnsupportedPixelFormat(format!(
            "depth={} bpp={} byte_order={:?}",
            reply.depth, format.bits_per_pixel, setup.image_byte_order
        )));
    }

    let width = rect.width as usize;
    let height = rect.height as usize;
    let pad = format.scanline_pad as usize;
    let stride_bits = width * format.bits_per_pixel as usize;
    let stride = stride_bits.div_ceil(pad) * pad / 8;

    if reply.data.len() < stride * height {
        return Err(PlatformError::Capture(format!(
            "截图数据不足: {} < {}",
            reply.data.len(),
            stride * height
        )));
    }

    let mut rgba = vec![0u8; width * height * 4];
    for row in 0..height {
        let src_row = &reply.data[row * stride..row * stride + width * 4];
        let dst_row = &mut rgba[row * width * 4..(row + 1) * width * 4];
        for (dst, src) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = 255;
        }
    }

    Ok(CaptureImage {
        width: rect.width,
        height: rect.height,
        origin_px: (rect.x, rect.y),
        rgba,
    })
}
