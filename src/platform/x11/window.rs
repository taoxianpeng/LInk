use std::collections::HashSet;

use gtk::gdk;
use gtk::glib::prelude::Cast as _;
use x11rb::connection::Connection;
use x11rb::protocol::shape::SK;
use x11rb::protocol::xfixes::ConnectionExt as XfixesConnectionExt;
use x11rb::protocol::xproto::{ConnectionExt as XprotoConnectionExt, Rectangle};
use x11rb::rust_connection::RustConnection;

use crate::platform::{PlatformError, RectI};

pub fn xid_of(window: &gdk::Window) -> Option<u32> {
    window
        .downcast_ref::<gdkx11::X11Window>()
        .map(|w| w.xid() as u32)
}

pub fn apply_input_region(
    conn: &RustConnection,
    windows: &[gdk::Window],
    passthrough: bool,
) -> Result<(), PlatformError> {
    apply_input_region_excluding(conn, windows, passthrough, None)
}

/// 与 [`apply_input_region`] 相同，但可以把 `exclude` 这块矩形从输入区域里挖掉
/// （画布用：面板那块留给面板，点击不会被全屏覆盖层吃掉）。
pub fn apply_input_region_excluding(
    conn: &RustConnection,
    windows: &[gdk::Window],
    passthrough: bool,
    exclude: Option<RectI>,
) -> Result<(), PlatformError> {
    let mut seen = HashSet::new();
    let mut applied = 0usize;
    let mut last_error = None;

    for window in windows {
        let Some(xid) = xid_of(window) else {
            last_error = Some(PlatformError::NoX11Window);
            continue;
        };
        if !seen.insert(xid) {
            continue;
        }
        match apply_one(conn, xid, passthrough, exclude) {
            Ok(()) => applied += 1,
            Err(err) => {
                log::warn!("XID {xid:#x} 应用输入区域失败: {err}");
                last_error = Some(err);
            }
        }
    }

    conn.flush()
        .map_err(|err| PlatformError::X11Request(err.to_string()))?;

    if applied == 0 {
        return Err(last_error.unwrap_or(PlatformError::NoX11Window));
    }
    Ok(())
}

fn apply_one(
    conn: &RustConnection,
    xid: u32,
    passthrough: bool,
    exclude: Option<RectI>,
) -> Result<(), PlatformError> {
    let region = conn
        .generate_id()
        .map_err(|err| PlatformError::X11Request(err.to_string()))?;
    log::debug!(
        "XFixes: xid={xid:#x} region={region:#x} 资源基址={:#x} 穿透={passthrough}",
        conn.setup().resource_id_base
    );

    let rectangles = if passthrough {
        Vec::new()
    } else {
        let geometry = conn
            .get_geometry(xid)
            .map_err(|err| PlatformError::X11Request(err.to_string()))?
            .reply()
            .map_err(|err| PlatformError::X11Request(err.to_string()))?;
        let full = Rectangle {
            x: 0,
            y: 0,
            width: geometry.width,
            height: geometry.height,
        };
        match exclude {
            Some(cut) => subtract_rect(full, cut),
            None => vec![full],
        }
    };

    conn.xfixes_create_region(region, &rectangles)
        .map_err(|err| PlatformError::X11Request(err.to_string()))?
        .check()
        .map_err(|err| {
            PlatformError::X11Request(format!(
                "CreateRegion(region={region:#x}, rects={}) 失败: {err:?}",
                rectangles.len()
            ))
        })?;

    conn.xfixes_set_window_shape_region(xid, SK::INPUT, 0, 0, region)
        .map_err(|err| PlatformError::X11Request(err.to_string()))?
        .check()
        .map_err(|err| {
            PlatformError::X11Request(format!(
                "SetWindowShapeRegion(window={xid:#x}, region={region:#x}) 失败: {err:?}"
            ))
        })?;

    conn.xfixes_destroy_region(region)
        .map_err(|err| PlatformError::X11Request(err.to_string()))?
        .check()
        .map_err(|err| {
            PlatformError::X11Request(format!("DestroyRegion(region={region:#x}) 失败: {err:?}"))
        })?;

    Ok(())
}

/// 用最多 4 个矩形表示「整块减去一块」。
fn subtract_rect(full: Rectangle, cut: RectI) -> Vec<Rectangle> {
    let fx0 = full.x as i32;
    let fy0 = full.y as i32;
    let fx1 = fx0 + full.width as i32;
    let fy1 = fy0 + full.height as i32;
    let cx0 = cut.x;
    let cy0 = cut.y;
    let cx1 = cx0 + cut.width as i32;
    let cy1 = cy0 + cut.height as i32;

    if cx1 <= fx0 || cy1 <= fy0 || cx0 >= fx1 || cy0 >= fy1 {
        return vec![full];
    }

    let mut out = Vec::new();
    let top = (cy0 - fy0).max(0);
    if top > 0 {
        out.push(Rectangle {
            x: fx0 as i16,
            y: fy0 as i16,
            width: full.width,
            height: top as u16,
        });
    }
    let bottom = (fy1 - cy1).max(0);
    if bottom > 0 {
        out.push(Rectangle {
            x: fx0 as i16,
            y: cy1 as i16,
            width: full.width,
            height: bottom as u16,
        });
    }
    let band_top = cy0.max(fy0);
    let band_height = (cy1.min(fy1) - band_top).max(0) as u16;
    let left = (cx0 - fx0).max(0);
    if left > 0 && band_height > 0 {
        out.push(Rectangle {
            x: fx0 as i16,
            y: band_top as i16,
            width: left as u16,
            height: band_height,
        });
    }
    let right = (fx1 - cx1).max(0);
    if right > 0 && band_height > 0 {
        out.push(Rectangle {
            x: cx1 as i16,
            y: band_top as i16,
            width: right as u16,
            height: band_height,
        });
    }
    out
}
