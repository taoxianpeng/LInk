//! Linux/X11 画布进程：GTK 透明覆盖层 + 压感输入，通过 IPC（stdin/stdout，JSON lines）被面板驱动。
//!
//! 与 v0.1 的 `main.rs` 区别：这里**没有工具窗**，所有 UI 都在 egui 面板进程里。

use std::cell::{Cell, RefCell};
use std::io::{self, BufReader};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use link_ipc::{read_command, write_event, Command, Event};

use link_ink::canvas_app::{CanvasApp, CanvasHost};
use link_ink::damage::{merge_scope, union_rect, DamageRect, DrawScope};
use link_ink::input::InputEvent;
use link_ink::paint::CairoCanvas;
use link_ink::platform::x11::window::xid_of;
use link_ink::platform::x11::{overlay, X11Platform};
use link_ink::platform::{CaptureImage, PlatformError, RectI};

/// 隐藏覆盖层后等待合成器出帧的时间（约两帧 @60Hz）。
const CAPTURE_DELAY: Duration = Duration::from_millis(150);

struct GtkCanvasHost {
    platform: Rc<X11Platform>,
    window: gtk::Window,
    area: gtk::DrawingArea,
    passthrough: Rc<Cell<bool>>,
}

impl GtkCanvasHost {
    fn drain_events(&self) {
        for _ in 0..1000 {
            if !gtk::events_pending() {
                break;
            }
            gtk::main_iteration_do(false);
        }
        if let Some(display) = gdk::Display::default() {
            display.sync();
        }
    }
}

impl CanvasHost for GtkCanvasHost {
    fn set_click_through(&mut self, enabled: bool) -> Result<(), PlatformError> {
        self.passthrough.set(enabled);
        refresh_input_region_for(&self.platform, &self.window, &self.area, enabled)?;
        log::info!(
            "{}",
            if enabled {
                "穿透开启"
            } else {
                "穿透关闭"
            }
        );
        Ok(())
    }

    fn capture_primary_hidden(&mut self) -> Result<CaptureImage, PlatformError> {
        self.window.hide();
        self.drain_events();
        std::thread::sleep(CAPTURE_DELAY);
        let result = self.platform.capture_primary();
        self.window.show_all();
        self.drain_events();
        if let Err(error) = refresh_input_region_for(
            &self.platform,
            &self.window,
            &self.area,
            self.passthrough.get(),
        ) {
            log::warn!("恢复输入区域失败: {error}");
        }
        result
    }
}

fn emit(events: Vec<Event>) {
    if events.is_empty() {
        return;
    }
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    for event in events {
        if let Err(error) = write_event(&mut lock, &event) {
            log::error!("写 IPC 事件失败: {error}");
            break;
        }
    }
}

pub fn run() -> glib::ExitCode {
    link_ink::logfile::init("info");

    let app = gtk::Application::builder()
        .application_id("io.github.linkink.Canvas")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(|app| {
        if let Err(error) = build_canvas(app) {
            log::error!("画布启动失败: {error}");
            emit(vec![Event::Error {
                message: format!("画布启动失败：{error}"),
            }]);
            std::process::exit(1);
        }
    });
    app.run()
}

fn build_canvas(app: &gtk::Application) -> Result<(), PlatformError> {
    let platform = Rc::new(X11Platform::new()?);
    let monitor = *platform.monitor();
    log::info!(
        "画布：物理 {}x{}+{}+{} @ {:.2}x",
        monitor.physical.width,
        monitor.physical.height,
        monitor.physical.x,
        monitor.physical.y,
        monitor.scale
    );

    let canvas = Rc::new(RefCell::new(CanvasApp::new(monitor)));
    let area_slot: Rc<RefCell<Option<gtk::DrawingArea>>> = Rc::new(RefCell::new(None));
    let last_damage: Rc<Cell<Option<DamageRect>>> = Rc::new(Cell::new(None));
    let pending_draw: Rc<Cell<Option<DrawScope>>> = Rc::new(Cell::new(None));

    let on_event = {
        let canvas = canvas.clone();
        let area_slot = area_slot.clone();
        let last_damage = last_damage.clone();
        let pending_draw = pending_draw.clone();
        Rc::new(move |event: InputEvent| {
            let mut canvas = canvas.borrow_mut();
            // 注意顺序：脏区域必须**在处理事件之后**取，否则队列里是上一段的矩形，
            // 最新一小段笔迹这一帧不会被重绘（表现为笔尖掉队/闪烁）。
            let events = canvas.on_input(event);
            let needs_redraw = canvas.take_redraw();
            let force_full = canvas.app_mut().take_force_full_redraw();
            let full_redraw = force_full || canvas.app().is_erasing();
            let damage = if force_full {
                None
            } else {
                canvas.damage_to_device(canvas.app().interaction_damage_pt())
            };
            drop(canvas);

            emit(events);
            if needs_redraw || force_full {
                if let Some(area) = area_slot.borrow().as_ref() {
                    queue_damage(area, damage, full_redraw, &last_damage, &pending_draw);
                }
            }
        })
    };

    // GTK 的 cairo 上下文按逻辑像素工作（表面带 device_scale），
    // Canvas 抽象统一按设备像素绘制，所以这里把缩放因子传进去抵消。
    let device_scale = monitor.scale;
    let on_draw = {
        let canvas = canvas.clone();
        let pending_draw = pending_draw.clone();
        Rc::new(move |cr: &cairo::Context| {
            let scope = pending_draw.take();
            let mut ctx = CairoCanvas::from_ref(cr, device_scale);
            let mut canvas = canvas.borrow_mut();
            let (damage, clip) = match scope {
                Some(DrawScope::Partial((x0, y0, x1, y1))) => (
                    Some((x0, y0, x1, y1)),
                    link_ink::paint::RectF::from_bounds(x0, y0, x1, y1),
                ),
                Some(DrawScope::Full) | None => (None, canvas.full_clip()),
            };
            canvas.draw(&mut ctx, damage, clip);
        })
    };

    let overlay = overlay::create(app, &monitor, on_event, on_draw)?;
    *area_slot.borrow_mut() = Some(overlay.area.clone());

    let passthrough = Rc::new(Cell::new(false));
    let host = GtkCanvasHost {
        platform: platform.clone(),
        window: overlay.window.clone(),
        area: overlay.area.clone(),
        passthrough: passthrough.clone(),
    };
    let host = Rc::new(RefCell::new(host));

    overlay.window.show_all();

    // 画布是后创建的全屏置顶窗口，会压在面板上面吞掉点击。
    // 做法：把画布的**输入区域**挖掉面板矩形——点到面板时事件直接落到面板，
    // 其余地方仍由画布接收。面板被拖动时靠下面的定时器重新计算。
    let _ = refresh_input_region_for(&platform, &overlay.window, &overlay.area, false);
    {
        let passthrough = passthrough.clone();
        let window = overlay.window.clone();
        let area = overlay.area.clone();
        let platform = platform.clone();
        glib::timeout_add_local(Duration::from_millis(400), move || {
            let _ = refresh_input_region_for(&platform, &window, &area, passthrough.get());
            glib::ControlFlow::Continue
        });
    }

    // 让画布窗口保持可见但不要抢焦点（面板才是交互入口）。
    overlay.window.set_can_focus(false);
    overlay.window.set_accept_focus(false);

    // 不在启动时推送状态：等面板发 Hello 再回 ready+state，
    // 否则面板会先收到一批"无人认领"的事件，容易和握手响应混在一起。

    // ---- IPC：stdin 读命令 → 主线程处理 ----
    let (tx, rx) = mpsc::channel::<Command>();
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        loop {
            match read_command(&mut reader) {
                Ok(Some(command)) => {
                    let quit = matches!(command, Command::Quit);
                    if tx.send(command).is_err() {
                        break;
                    }
                    if quit {
                        break;
                    }
                }
                Ok(None) => {
                    log::info!("面板已关闭（stdin EOF），画布退出");
                    break;
                }
                Err(error) => {
                    log::warn!("IPC 解析失败: {error}");
                }
            }
        }
        // 让主循环感知到 EOF。
        let _ = tx.send(Command::Quit);
    });

    {
        let canvas = canvas.clone();
        let host = host.clone();
        let area = overlay.area.clone();
        let last_damage = last_damage.clone();
        let application = app.clone();
        glib::timeout_add_local(Duration::from_millis(8), move || {
            while let Ok(command) = rx.try_recv() {
                if matches!(command, Command::Quit) {
                    log::info!("收到退出命令");
                    application.quit();
                    return glib::ControlFlow::Break;
                }
                log::debug!("画布收到命令: {command:?}");
                let events = canvas
                    .borrow_mut()
                    .handle_command(command, &mut *host.borrow_mut());
                let needs_redraw = canvas.borrow_mut().take_redraw();
                // 命令路径本身就整屏重绘，清掉输入路径的一次性标志。
                let _ = canvas.borrow_mut().app_mut().take_force_full_redraw();
                emit(events);
                if needs_redraw {
                    last_damage.set(None);
                    pending_draw.set(Some(DrawScope::Full));
                    area.queue_draw();
                }
            }
            glib::ControlFlow::Continue
        });
    }

    Ok(())
}

/// 把画布的输入区域设为「整屏 − 面板矩形」（穿透时为空）。
///
/// 画布盖在面板之上，但输入区域不含面板那块，点击就穿透到面板；
/// 视觉上画布是透明的，面板依然可见。
fn refresh_input_region_for(
    platform: &X11Platform,
    overlay: &gtk::Window,
    area: &gtk::DrawingArea,
    passthrough: bool,
) -> Result<(), PlatformError> {
    let mut windows = Vec::new();
    if let Some(window) = overlay.window() {
        windows.push(window);
    }
    if let Some(window) = area.window() {
        windows.push(window);
    }
    if windows.is_empty() {
        return Ok(());
    }
    let exclude = if passthrough {
        None
    } else {
        overlay
            .window()
            .and_then(|window| xid_of(&window))
            .and_then(panel_rect_in_overlay)
    };
    if let Some(rect) = exclude {
        log::debug!(
            "输入区域排除面板：({},{}) {}x{}",
            rect.x,
            rect.y,
            rect.width,
            rect.height
        );
    }
    platform.apply_input_region_excluding(&windows, passthrough, exclude)
}

/// 面板窗口在画布坐标系里的矩形。
fn panel_rect_in_overlay(overlay_xid: u32) -> Option<RectI> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};

    let (connection, screen) = x11rb::connect(None).ok()?;
    let root = connection.setup().roots[screen].root;
    let atom = connection
        .intern_atom(false, b"_NET_CLIENT_LIST")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let reply = connection
        .get_property(false, root, atom, AtomEnum::WINDOW, 0, 64)
        .ok()?
        .reply()
        .ok()?;
    let windows: Vec<u32> = reply.value32()?.collect();

    for window in windows {
        if window == overlay_xid {
            continue;
        }
        let Ok(cookie) =
            connection.get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 32)
        else {
            continue;
        };
        let Ok(class) = cookie.reply() else {
            continue;
        };
        let class = String::from_utf8_lossy(&class.value);
        if !(class.contains("LInk") || class.contains("link-ink-panel")) {
            continue;
        }
        let geometry = connection.get_geometry(window).ok()?.reply().ok()?;
        let translated = connection
            .translate_coordinates(window, overlay_xid, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        return Some(RectI {
            x: translated.dst_x as i32,
            y: translated.dst_y as i32,
            width: geometry.width as u32,
            height: geometry.height as u32,
        });
    }
    None
}

/// 脏矩形重绘：自由笔迹只刷新最后一段，避免长笔迹每帧重画整条。
///
/// 这里的矩形已经是**设备像素**（`CanvasApp::damage_to_device` 换算过）。
fn queue_damage(
    area: &gtk::DrawingArea,
    damage: Option<DamageRect>,
    full_redraw: bool,
    last_damage: &Rc<Cell<Option<DamageRect>>>,
    pending_draw: &Rc<Cell<Option<DrawScope>>>,
) {
    let previous = last_damage.replace(damage);
    if full_redraw {
        pending_draw.set(Some(DrawScope::Full));
        area.queue_draw();
        return;
    }
    let Some(rect) = union_rect(damage, previous) else {
        log::trace!("脏矩形: 无（回退整屏）");
        pending_draw.set(Some(DrawScope::Full));
        area.queue_draw();
        return;
    };
    let (x, y, width, height) = link_ink::damage::integer_rect(rect);
    log::trace!("脏矩形: ({x},{y}) {width}x{height}");
    let scope = DrawScope::Partial((rect.0 - 2.0, rect.1 - 2.0, rect.2 + 2.0, rect.3 + 2.0));
    pending_draw.set(Some(merge_scope(pending_draw.get(), scope)));
    area.queue_draw_area(x, y, width, height);
}
