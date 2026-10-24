pub mod capture;
pub mod input;
pub mod overlay;
pub mod window;

use std::cell::Cell;

use gtk::gdk;
use gtk::gdk::prelude::*;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as RandrConnectionExt;
use x11rb::protocol::xfixes::ConnectionExt as XfixesConnectionExt;
use x11rb::protocol::xproto::ConnectionExt as XprotoConnectionExt;
use x11rb::rust_connection::RustConnection;

use crate::platform::{CaptureImage, MonitorInfo, PlatformError, RectF, RectI};

pub struct X11Platform {
    conn: RustConnection,
    screen_num: usize,
    monitor: MonitorInfo,
    passthrough: Cell<bool>,
}

impl X11Platform {
    pub fn new() -> Result<Self, PlatformError> {
        let (conn, screen_num) =
            x11rb::connect(None).map_err(|err| PlatformError::X11Connect(err.to_string()))?;

        let (logical, gdk_scale) = gdk_monitor_info()?;
        let physical = primary_monitor_rect(&conn, screen_num).unwrap_or_else(|err| {
            log::warn!("XRandR 主屏查询失败，按 GDK 逻辑几何推算: {err}");
            RectI {
                x: (logical.x * gdk_scale).round() as i32,
                y: (logical.y * gdk_scale).round() as i32,
                width: (logical.width * gdk_scale).round() as u32,
                height: (logical.height * gdk_scale).round() as u32,
            }
        });

        let scale = if logical.width > 0.0 {
            physical.width as f64 / logical.width
        } else {
            gdk_scale
        };
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };

        let monitor = MonitorInfo {
            physical,
            logical,
            scale,
        };

        log::info!(
            "主屏: 物理 {}x{}+{}+{}, 逻辑 {:.1}x{:.1}+{:.1}+{:.1}, scale {:.3} (GDK {:.3})",
            monitor.physical.width,
            monitor.physical.height,
            monitor.physical.x,
            monitor.physical.y,
            monitor.logical.width,
            monitor.logical.height,
            monitor.logical.x,
            monitor.logical.y,
            monitor.scale,
            gdk_scale
        );

        let expected_h = monitor.physical.height as f64;
        let actual_h = monitor.logical.height * monitor.scale;
        if (expected_h - actual_h).abs() > 1.0 {
            log::warn!(
                "物理/逻辑高度比例不一致: 物理 {:.1}, 逻辑×scale {:.1}",
                expected_h,
                actual_h
            );
        }

        // Xorg 要求先 QueryVersion，否则后续 CreateRegion 会被拒绝（BadRequest）
        match conn
            .xfixes_query_version(6, 0)
            .map_err(|err| err.to_string())
            .and_then(|cookie| cookie.reply().map_err(|err| err.to_string()))
        {
            Ok(reply) => log::info!(
                "XFixes 版本 {}.{}",
                reply.major_version,
                reply.minor_version
            ),
            Err(err) => log::warn!("XFixes 版本查询失败: {err}"),
        }

        Ok(Self {
            conn,
            screen_num,
            monitor,
            passthrough: Cell::new(false),
        })
    }

    pub fn monitor(&self) -> &MonitorInfo {
        &self.monitor
    }

    pub fn is_passthrough(&self) -> bool {
        self.passthrough.get()
    }

    pub fn set_click_through(
        &self,
        windows: &[gdk::Window],
        enabled: bool,
    ) -> Result<(), PlatformError> {
        window::apply_input_region(&self.conn, windows, enabled)?;
        self.passthrough.set(enabled);
        Ok(())
    }

    pub fn reapply_input_region(&self, windows: &[gdk::Window]) -> Result<(), PlatformError> {
        window::apply_input_region(&self.conn, windows, self.passthrough.get())
    }

    pub fn capture_primary(&self) -> Result<CaptureImage, PlatformError> {
        capture::capture(&self.conn, self.screen_num, self.monitor.physical)
    }

    pub fn screen_number(&self) -> usize {
        self.screen_num
    }

    pub fn window_geometry(&self, window: &gdk::Window) -> Result<RectI, PlatformError> {
        let xid = window::xid_of(window).ok_or(PlatformError::NoX11Window)?;
        let geometry = self
            .conn
            .get_geometry(xid)
            .map_err(|err| PlatformError::X11Request(err.to_string()))?
            .reply()
            .map_err(|err| PlatformError::X11Request(err.to_string()))?;
        Ok(RectI {
            x: geometry.x as i32,
            y: geometry.y as i32,
            width: geometry.width as u32,
            height: geometry.height as u32,
        })
    }

    pub fn window_root_position(&self, window: &gdk::Window) -> Result<(i32, i32), PlatformError> {
        let xid = window::xid_of(window).ok_or(PlatformError::NoX11Window)?;
        let root = self
            .conn
            .setup()
            .roots
            .get(self.screen_num)
            .map(|screen| screen.root)
            .ok_or(PlatformError::NoMonitor)?;
        let reply = self
            .conn
            .translate_coordinates(xid, root, 0, 0)
            .map_err(|err| PlatformError::X11Request(err.to_string()))?
            .reply()
            .map_err(|err| PlatformError::X11Request(err.to_string()))?;
        Ok((reply.dst_x as i32, reply.dst_y as i32))
    }

    pub fn log_stacking(&self, named_windows: &[(&str, &gdk::Window)]) {
        match self.stacking_order(named_windows) {
            Ok(order) => log::info!("窗口堆叠（底→顶）: {order}"),
            Err(err) => log::warn!("窗口堆叠查询失败: {err}"),
        }
    }

    fn stacking_order(
        &self,
        named_windows: &[(&str, &gdk::Window)],
    ) -> Result<String, PlatformError> {
        let root = self
            .conn
            .setup()
            .roots
            .get(self.screen_num)
            .map(|screen| screen.root)
            .ok_or(PlatformError::NoMonitor)?;
        let tree = self
            .conn
            .query_tree(root)
            .map_err(|err| PlatformError::X11Request(err.to_string()))?
            .reply()
            .map_err(|err| PlatformError::X11Request(err.to_string()))?;

        let mut entries: Vec<(String, usize)> = Vec::new();
        for (name, window) in named_windows {
            let Some(xid) = window::xid_of(window) else {
                continue;
            };
            let top = self.root_child(root, xid)?;
            match tree.children.iter().position(|child| *child == top) {
                Some(index) => entries.push(((*name).to_string(), index)),
                None => log::warn!("{name} 的顶层窗口 {top:#x} 不在根窗口子列表中"),
            }
        }
        entries.sort_by_key(|(_, index)| *index);
        Ok(entries
            .iter()
            .map(|(name, index)| format!("{name}@{index}"))
            .collect::<Vec<_>>()
            .join(" < "))
    }

    fn root_child(&self, root: u32, xid: u32) -> Result<u32, PlatformError> {
        let mut current = xid;
        for _ in 0..32 {
            let tree = self
                .conn
                .query_tree(current)
                .map_err(|err| PlatformError::X11Request(err.to_string()))?
                .reply()
                .map_err(|err| PlatformError::X11Request(err.to_string()))?;
            if tree.parent == root || tree.parent == 0 {
                return Ok(current);
            }
            current = tree.parent;
        }
        Err(PlatformError::X11Request(format!(
            "窗口 {xid:#x} 的根祖先查找层数过深"
        )))
    }
}

fn gdk_monitor_info() -> Result<(RectF, f64), PlatformError> {
    let display = gdk::Display::default().ok_or(PlatformError::NoMonitor)?;
    let monitor = display
        .primary_monitor()
        .or_else(|| display.monitor(0))
        .ok_or(PlatformError::NoMonitor)?;
    let geometry = monitor.geometry();
    let scale = monitor.scale_factor() as f64;
    Ok((
        RectF {
            x: geometry.x() as f64,
            y: geometry.y() as f64,
            width: geometry.width() as f64,
            height: geometry.height() as f64,
        },
        if scale > 0.0 { scale } else { 1.0 },
    ))
}

fn primary_monitor_rect(conn: &RustConnection, screen_num: usize) -> Result<RectI, PlatformError> {
    let root = conn
        .setup()
        .roots
        .get(screen_num)
        .map(|screen| screen.root)
        .ok_or(PlatformError::NoMonitor)?;

    conn.randr_query_version(1, 6)
        .map_err(|err| PlatformError::X11Request(err.to_string()))?
        .reply()
        .map_err(|err| PlatformError::X11Request(err.to_string()))?;

    let reply = conn
        .randr_get_monitors(root, true)
        .map_err(|err| PlatformError::X11Request(err.to_string()))?
        .reply()
        .map_err(|err| PlatformError::X11Request(err.to_string()))?;

    let monitor = reply
        .monitors
        .iter()
        .find(|monitor| monitor.primary)
        .or_else(|| reply.monitors.first())
        .ok_or(PlatformError::NoMonitor)?;

    Ok(RectI {
        x: monitor.x as i32,
        y: monitor.y as i32,
        width: monitor.width as u32,
        height: monitor.height as u32,
    })
}

pub fn compositor_active() -> bool {
    gdk::Display::default()
        .map(|display| display.default_screen().is_composited())
        .unwrap_or(false)
}
