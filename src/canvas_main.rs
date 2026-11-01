//! 原生画布进程：GTK 透明覆盖层 + 压感输入，通过 IPC（stdin/stdout，JSON lines）被面板驱动。
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
use link_ink::input::InputEvent;
use link_ink::model::Point;
use link_ink::platform::x11::window::xid_of;
use link_ink::platform::x11::{overlay, X11Platform};
use link_ink::platform::{CaptureImage, PlatformError, RectI};
use link_ink::view::ViewTransform;

/// 隐藏覆盖层后等待合成器出帧的时间（约两帧 @60Hz）。
const CAPTURE_DELAY: Duration = Duration::from_millis(150);

type DamageRect = (f64, f64, f64, f64);

/// 下一次绘制要覆盖的范围。
///
/// GTK 会把多个指针事件合并成**一次**绘制，所以这里必须累加：只保留最后一帧的矩形
/// 会让更早那些图形留下的像素永远没被擦掉（形状工具拖动时就是一片残影）。
#[derive(Debug, Clone, Copy, PartialEq)]
enum DrawScope {
    /// 整屏重绘（图层操作、擦除、导出恢复……），裁剪交给 GTK。
    Full,
    /// 只重绘这一块（逻辑坐标）。
    Partial(DamageRect),
}

fn merge_scope(previous: Option<DrawScope>, next: DrawScope) -> DrawScope {
    match (previous, next) {
        // 只要有一次要求整屏，就必须整屏。
        (Some(DrawScope::Full), _) | (_, DrawScope::Full) => DrawScope::Full,
        (Some(DrawScope::Partial(a)), DrawScope::Partial(b)) => {
            DrawScope::Partial((a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
        }
        (None, next) => next,
    }
}

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

fn main() -> glib::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

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
                canvas.app().interaction_damage_pt()
            };
            let view = canvas.view();
            drop(canvas);

            emit(events);
            if needs_redraw || force_full {
                if let Some(area) = area_slot.borrow().as_ref() {
                    queue_damage(
                        area,
                        &view,
                        damage,
                        full_redraw,
                        &last_damage,
                        &pending_draw,
                    );
                }
            }
        })
    };

    let on_draw = {
        let canvas = canvas.clone();
        let pending_draw = pending_draw.clone();
        Rc::new(move |cr: &cairo::Context| {
            let damage = match pending_draw.take() {
                Some(DrawScope::Partial(rect)) => Some(rect),
                Some(DrawScope::Full) | None => None,
            };
            canvas.borrow_mut().draw(cr, damage);
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
fn queue_damage(
    area: &gtk::DrawingArea,
    view: &ViewTransform,
    damage_page: Option<DamageRect>,
    full_redraw: bool,
    last_damage: &Rc<Cell<Option<DamageRect>>>,
    pending_draw: &Rc<Cell<Option<DrawScope>>>,
) {
    let current = damage_page.map(|(x0, y0, x1, y1)| {
        let (ax, ay) = view.page_to_logical(Point::new(x0, y0));
        let (bx, by) = view.page_to_logical(Point::new(x1, y1));
        (ax.min(bx), ay.min(by), ax.max(bx), ay.max(by))
    });
    let previous = last_damage.replace(current);
    if full_redraw {
        pending_draw.set(Some(DrawScope::Full));
        area.queue_draw();
        return;
    }
    let Some(rect) = union_rect(current, previous) else {
        log::trace!("脏矩形: 无（回退整屏）");
        pending_draw.set(Some(DrawScope::Full));
        area.queue_draw();
        return;
    };
    let (x, y, width, height) = integer_rect(rect);
    log::trace!("脏矩形: ({x},{y}) {width}x{height}");
    let scope = DrawScope::Partial((rect.0 - 2.0, rect.1 - 2.0, rect.2 + 2.0, rect.3 + 2.0));
    pending_draw.set(Some(merge_scope(pending_draw.get(), scope)));
    area.queue_draw_area(x, y, width, height);
}

fn union_rect(a: Option<DamageRect>, b: Option<DamageRect>) -> Option<DamageRect> {
    match (a, b) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn integer_rect(rect: DamageRect) -> (i32, i32, i32, i32) {
    let x0 = rect.0.floor() as i32 - 1;
    let y0 = rect.1.floor() as i32 - 1;
    let x1 = rect.2.ceil() as i32 + 1;
    let y1 = rect.3.ceil() as i32 + 1;
    (x0, y0, (x1 - x0).max(1), (y1 - y0).max(1))
}

#[cfg(test)]
mod draw_scope_tests {
    use super::{merge_scope, DrawScope};

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> DrawScope {
        DrawScope::Partial((x0, y0, x1, y1))
    }

    #[test]
    fn partial_damages_accumulate_instead_of_replacing() {
        // 两个事件在一次绘制前到达：必须取并集，否则前一个图形的像素不会被擦掉。
        let first = merge_scope(None, rect(0.0, 0.0, 10.0, 10.0));
        let both = merge_scope(Some(first), rect(20.0, 20.0, 30.0, 30.0));
        assert_eq!(both, rect(0.0, 0.0, 30.0, 30.0));
    }

    #[test]
    fn full_repaint_wins_over_partial() {
        let partial = merge_scope(None, rect(0.0, 0.0, 10.0, 10.0));
        assert_eq!(merge_scope(Some(partial), DrawScope::Full), DrawScope::Full);
        assert_eq!(
            merge_scope(Some(DrawScope::Full), rect(5.0, 5.0, 6.0, 6.0)),
            DrawScope::Full
        );
    }

    #[test]
    fn first_scope_is_kept_as_is() {
        assert_eq!(
            merge_scope(None, rect(1.0, 2.0, 3.0, 4.0)),
            rect(1.0, 2.0, 3.0, 4.0)
        );
    }
}
