use std::rc::Rc;

use gtk::gdk;
use gtk::gdk::prelude::*;
use gtk::glib;
use gtk::prelude::*;

use crate::input::InputEvent;
use crate::platform::x11::input::{has_pressure_axis, sample_from_event};
use crate::platform::{MonitorInfo, PlatformError};

pub struct OverlayWindow {
    pub window: gtk::Window,
    pub area: gtk::DrawingArea,
}

pub fn create(
    app: &gtk::Application,
    monitor: &MonitorInfo,
    on_event: Rc<dyn Fn(InputEvent)>,
    on_draw: Rc<dyn Fn(&cairo::Context)>,
) -> Result<OverlayWindow, PlatformError> {
    install_css();

    let logical_width = monitor.logical.width.round().max(1.0) as i32;
    let logical_height = monitor.logical.height.round().max(1.0) as i32;
    let origin_x = monitor.logical.x.round() as i32;
    let origin_y = monitor.logical.y.round() as i32;

    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_application(Some(app));
    window.set_widget_name("link-overlay");
    window.set_title("LInk 标注层");
    window.set_decorated(false);
    window.set_resizable(false);
    window.set_can_focus(false);
    window.set_accept_focus(false);
    window.set_focus_on_map(false);
    window.set_skip_taskbar_hint(true);
    window.set_skip_pager_hint(true);
    window.set_keep_above(true);
    window.set_app_paintable(true);
    window.set_default_size(logical_width, logical_height);
    window.set_size_request(logical_width, logical_height);
    window.move_(origin_x, origin_y);

    let display = gdk::Display::default().ok_or(PlatformError::NoMonitor)?;
    let visual = display
        .default_screen()
        .rgba_visual()
        .ok_or(PlatformError::TransparencyUnsupported)?;
    window.set_visual(Some(&visual));

    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.add_events(
        gdk::EventMask::BUTTON_PRESS_MASK
            | gdk::EventMask::BUTTON_RELEASE_MASK
            | gdk::EventMask::POINTER_MOTION_MASK
            | gdk::EventMask::PROXIMITY_IN_MASK
            | gdk::EventMask::PROXIMITY_OUT_MASK,
    );
    area.connect_realize(|widget| {
        if let Some(window) = widget.window() {
            window.set_support_multidevice(true);
        }
    });
    window.add(&area);

    {
        let on_draw = on_draw.clone();
        area.connect_draw(move |_, cr| {
            on_draw(cr);
            glib::Propagation::Proceed
        });
    }

    {
        let on_event = on_event.clone();
        area.connect_button_press_event(move |_, event| {
            if event.button() == 1 {
                on_event(InputEvent::Down(sample_from_event(event, event.position())));
            }
            glib::Propagation::Stop
        });
    }
    {
        let on_event = on_event.clone();
        area.connect_motion_notify_event(move |_, event| {
            on_event(InputEvent::Move(sample_from_event(event, event.position())));
            glib::Propagation::Stop
        });
    }
    {
        let on_event = on_event.clone();
        area.connect_button_release_event(move |_, event| {
            if event.button() == 1 {
                on_event(InputEvent::Up(sample_from_event(event, event.position())));
            }
            glib::Propagation::Stop
        });
    }
    {
        area.connect_proximity_in_event(move |_, event| {
            log::debug!("笔进入感应区: {:?}", event.device().and_then(|d| d.name()));
            glib::Propagation::Proceed
        });
    }
    {
        let on_event = on_event.clone();
        area.connect_proximity_out_event(move |_, _| {
            on_event(InputEvent::ProximityOut);
            glib::Propagation::Proceed
        });
    }

    let manager = display
        .default_seat()
        .and_then(|seat| seat.pointer())
        .and_then(|pointer| pointer.device_manager());
    if let Some(manager) = manager {
        {
            let on_event = on_event.clone();
            manager.connect_device_removed(move |_, device| {
                log::info!(
                    "设备移除: {}",
                    device
                        .name()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "未知".to_string())
                );
                on_event(InputEvent::Cancel);
            });
        }
        manager.connect_device_added(move |_, device| {
            log::info!(
                "设备接入: {} · 来源={:?} · 压感轴={}",
                device
                    .name()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "未知".to_string()),
                device.source(),
                has_pressure_axis(device)
            );
        });
    }

    Ok(OverlayWindow { window, area })
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    if provider
        .load_from_data(
            b"#link-overlay, #link-overlay drawingarea { background-color: transparent; }",
        )
        .is_err()
    {
        log::warn!("覆盖层 CSS 加载失败");
        return;
    }
    if let Some(display) = gdk::Display::default() {
        gtk::StyleContext::add_provider_for_screen(
            &display.default_screen(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
