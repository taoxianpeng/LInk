use std::collections::HashSet;

use gtk::gdk;
use gtk::glib::prelude::Cast as _;
use x11rb::connection::Connection;
use x11rb::protocol::shape::SK;
use x11rb::protocol::xfixes::ConnectionExt as XfixesConnectionExt;
use x11rb::protocol::xproto::{ConnectionExt as XprotoConnectionExt, Rectangle};
use x11rb::rust_connection::RustConnection;

use crate::platform::PlatformError;

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
        match apply_one(conn, xid, passthrough) {
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

fn apply_one(conn: &RustConnection, xid: u32, passthrough: bool) -> Result<(), PlatformError> {
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
        vec![Rectangle {
            x: 0,
            y: 0,
            width: geometry.width,
            height: geometry.height,
        }]
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
