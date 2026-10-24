use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use link_ink::app::App;
use link_ink::cache::OverlayCache;
use link_ink::model::{Document, Point};
use link_ink::platform::{
    compositor_active, overlay, CaptureImage, MonitorInfo, Platform, PlatformError,
};
use link_ink::ui;
use link_ink::ui::toolbar::StatusHandle;
use link_ink::view::ViewTransform;

thread_local! {
    static ACTIVE_UI: RefCell<Option<Rc<UiWindows>>> = const { RefCell::new(None) };
}

/// 逻辑坐标下的脏矩形 `(x0, y0, x1, y1)`。
type DamageRect = (f64, f64, f64, f64);
/// 上一帧草稿占用的区域（下一帧要从缓存里重刷掉）。
type DamageCell = Rc<Cell<Option<DamageRect>>>;

/// 隐藏窗口后等待合成器出帧的时间（约两帧 @60Hz），避免截图里残留自身窗口。
const SCREEN_CAPTURE_DELAY_MS: u64 = 150;

/// 排空 GTK 事件时的最大迭代次数，防止异常情况下事件循环不收敛。
const MAX_EVENT_DRAIN_ITERATIONS: usize = 1000;

struct UiWindows {
    overlay: gtk::Window,
    toolbar: gtk::Window,
}

/// 截图期间临时隐藏覆盖层与工具窗，离开作用域（含错误路径）时恢复。
struct HiddenWindows {
    overlay: gtk::Window,
    toolbar: Option<gtk::Window>,
}

impl HiddenWindows {
    fn hide(overlay: &gtk::Window, toolbar: Option<gtk::Window>) -> Self {
        overlay.hide();
        if let Some(toolbar) = &toolbar {
            toolbar.hide();
        }
        drain_events();
        Self {
            overlay: overlay.clone(),
            toolbar,
        }
    }
}

impl Drop for HiddenWindows {
    fn drop(&mut self) {
        self.overlay.show_all();
        if let Some(toolbar) = &self.toolbar {
            toolbar.show_all();
            toolbar.present();
        }
        drain_events();
    }
}

/// 处理掉挂起事件并把请求刷给 X server，保证 hide/show 真正生效后再截图。
///
/// 迭代次数设上限：万一某个控件持续排队（动画、重复 queue_draw），
/// 也不能让导出流程卡死在事件循环里。
/// 把本帧脏区域排进重绘队列。
///
/// 草稿每帧只新增一小段，整屏 `queue_draw()` 会让 4K 屏每帧都重画整张画布（1000 笔约 34ms），
/// 画面会闪。这里只排「上一帧草稿区域 ∪ 本帧草稿区域」，其余像素保持不动，
/// 绘制时再从离屏缓存中贴回来。
fn queue_overlay_damage(
    area: &gtk::DrawingArea,
    view: &ViewTransform,
    damage_page: Option<DamageRect>,
    erasing: bool,
    last_damage: &DamageCell,
) {
    let current = damage_page.map(|(x0, y0, x1, y1)| {
        let (ax, ay) = view.page_to_logical(Point::new(x0, y0));
        let (bx, by) = view.page_to_logical(Point::new(x1, y1));
        (ax.min(bx), ay.min(by), ax.max(bx), ay.max(by))
    });
    let previous = last_damage.replace(current);

    // 擦除会整条删除笔迹，脏区域无法预知，只能整屏重绘。
    if erasing {
        area.queue_draw();
        return;
    }

    let Some(rect) = union_rect(current, previous) else {
        area.queue_draw();
        return;
    };
    let (x, y, width, height) = integer_rect(rect);
    area.queue_draw_area(x, y, width, height);
}

/// 向外取整成 `queue_draw_area` 用的整数矩形，并多留 1px 容纳抗锯齿。
fn integer_rect(rect: DamageRect) -> (i32, i32, i32, i32) {
    let x0 = rect.0.floor() as i32 - 1;
    let y0 = rect.1.floor() as i32 - 1;
    let x1 = rect.2.ceil() as i32 + 1;
    let y1 = rect.3.ceil() as i32 + 1;
    (x0, y0, (x1 - x0).max(1), (y1 - y0).max(1))
}

fn union_rect(a: Option<DamageRect>, b: Option<DamageRect>) -> Option<DamageRect> {
    match (a, b) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn drain_events() {
    for _ in 0..MAX_EVENT_DRAIN_ITERATIONS {
        if !gtk::events_pending() {
            break;
        }
        gtk::main_iteration_do(false);
    }
    if let Some(display) = gdk::Display::default() {
        display.sync();
    }
}

fn main() -> glib::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let app = gtk::Application::builder()
        .application_id("io.github.linkink.LInk")
        .build();

    app.connect_activate(|app| {
        if let Err(err) = build_ui(app) {
            log::error!("启动失败: {err}");
            show_error(None, "LInk 启动失败", &err.to_string());
        }
    });

    app.run()
}

fn build_ui(app: &gtk::Application) -> Result<(), PlatformError> {
    if let Some(existing) = ACTIVE_UI.with(|slot| slot.borrow().clone()) {
        existing.overlay.present();
        existing.toolbar.present();
        existing.toolbar.present();
        return Ok(());
    }

    let platform = Rc::new(Platform::new()?);
    let monitor = *platform.monitor();
    let composited = compositor_active();
    if !composited {
        log::warn!("未检测到合成器，透明覆盖层可能显示为黑色");
    }

    let page_width = monitor.width_pt();
    let page_height = monitor.height_pt();
    log::info!(
        "页面 {:.2}x{:.2} pt · 逻辑 {:.1}x{:.1} · 物理 {}x{} @ {:.2}x",
        page_width,
        page_height,
        monitor.logical.width,
        monitor.logical.height,
        monitor.physical.width,
        monitor.physical.height,
        monitor.scale
    );

    let document = Document::new(page_width, page_height);
    let view = Rc::new(RefCell::new(ViewTransform::for_monitor(
        &monitor,
        page_width,
        page_height,
    )));
    let state = Rc::new(RefCell::new(App::new(document)));

    let status_slot: Rc<RefCell<Option<StatusHandle>>> = Rc::new(RefCell::new(None));
    let area_slot: Rc<RefCell<Option<gtk::DrawingArea>>> = Rc::new(RefCell::new(None));
    let passing = Rc::new(Cell::new(false));
    // 缓存延迟到首帧创建：设备缩放直接取目标绘制表面，避免与显示器缩放推算值不一致。
    let cache: Rc<RefCell<Option<OverlayCache>>> = Rc::new(RefCell::new(None));
    // 上一帧草稿占用的逻辑坐标区域：下一帧要把它从缓存里重刷掉
    let last_damage: DamageCell = Rc::new(Cell::new(None));

    let on_event = {
        let state = state.clone();
        let status_slot = status_slot.clone();
        let area_slot = area_slot.clone();
        let view = view.clone();
        let last_damage = last_damage.clone();
        Rc::new(move |event| {
            let view = *view.borrow();
            let (needs_redraw, erasing, damage) = {
                let mut app = state.borrow_mut();
                let needs_redraw = app.handle_input(event, &view);
                (needs_redraw, app.is_erasing(), app.draft_damage_pt())
            };
            if needs_redraw {
                if let Some(area) = area_slot.borrow().as_ref() {
                    queue_overlay_damage(area, &view, damage, erasing, &last_damage);
                }
            }
            if let Some(status) = status_slot.borrow().as_ref() {
                status.refresh_state(&state);
            }
        })
    };

    let on_draw = {
        let state = state.clone();
        let view = view.clone();
        let cache = cache.clone();
        Rc::new(move |cr: &cairo::Context| {
            let mut slot = cache.borrow_mut();
            let cache = slot.get_or_insert_with(|| {
                let scale = cr.target().device_scale().0;
                OverlayCache::new(&monitor, scale)
            });
            let app = state.borrow();
            let view = *view.borrow();
            ui::overlay::draw(cr, &app, &view, cache);
        })
    };

    let overlay = overlay::create(app, &monitor, on_event, on_draw)?;
    *area_slot.borrow_mut() = Some(overlay.area.clone());

    let on_passthrough = {
        let platform = platform.clone();
        let overlay_window = overlay.window.clone();
        let area = overlay.area.clone();
        let passing = passing.clone();
        let status_slot = status_slot.clone();
        Rc::new(move |enabled: bool| {
            let windows = input_windows(&overlay_window, &area);
            match platform.set_click_through(&windows, enabled) {
                Ok(()) => {
                    passing.set(enabled);
                    log::info!(
                        "{}",
                        if enabled {
                            "穿透开启"
                        } else {
                            "穿透关闭"
                        }
                    );
                }
                Err(err) => {
                    log::error!("切换穿透失败: {err}");
                    if let Some(status) = status_slot.borrow().as_ref() {
                        status.set_passthrough_state(!enabled);
                    }
                }
            }
        })
    };

    let redraw = {
        let area = overlay.area.clone();
        Rc::new(move || area.queue_draw())
    };
    let on_quit = {
        let app = app.clone();
        Rc::new(move || app.quit())
    };
    let on_document_loaded = {
        let state = state.clone();
        let view = view.clone();
        let cache = cache.clone();
        let last_damage = last_damage.clone();
        Rc::new(move || {
            // 载入的工程可能与旧文档结构完全相同，指纹无法区分，直接失效。
            if let Some(cache) = cache.borrow_mut().as_mut() {
                cache.invalidate();
            }
            last_damage.set(None);
            let app = state.borrow();
            let mut view = view.borrow_mut();
            *view =
                ViewTransform::for_monitor(&monitor, app.document.width_pt, app.document.height_pt);
        })
    };
    // 屏幕截图：隐藏自身窗口 → 排空事件并同步 → 等合成器出帧 → 抓取主屏像素 → 恢复窗口。
    // 隐藏/恢复由这里的 RAII guard 保证，出错路径也不会把覆盖层永久藏起来。
    let capture_screen: Rc<dyn Fn() -> Result<CaptureImage, PlatformError>> = {
        let platform = platform.clone();
        let overlay_window = overlay.window.clone();
        let area = overlay.area.clone();
        let passing = passing.clone();
        Rc::new(move || {
            let toolbar_window = ACTIVE_UI.with(|slot| {
                slot.borrow()
                    .as_ref()
                    .map(|windows| windows.toolbar.clone())
            });
            let guard = HiddenWindows::hide(&overlay_window, toolbar_window);
            std::thread::sleep(std::time::Duration::from_millis(SCREEN_CAPTURE_DELAY_MS));
            let result = platform.capture_primary();
            drop(guard);
            if passing.get() {
                reapply(&platform, &overlay_window, &area);
            }
            result
        })
    };
    let toolbar = ui::toolbar::build(
        app,
        state.clone(),
        ui::toolbar::ToolbarHooks {
            redraw,
            on_passthrough,
            on_quit,
            on_document_loaded,
            capture_screen,
        },
        monitor,
    );
    toolbar.window.set_transient_for(Some(&overlay.window));
    *status_slot.borrow_mut() = Some(toolbar.status.clone());

    install_region_reapply(&platform, &overlay.window, &overlay.area, passing.clone());
    log_window_geometry_once(platform.clone(), monitor, overlay.window.clone());

    overlay.window.show_all();
    toolbar.window.show_all();
    toolbar.window.present();

    {
        let toolbar_window = toolbar.window.clone();
        overlay
            .window
            .connect_map(move |_| toolbar_window.present());
    }

    ACTIVE_UI.with(|slot| {
        *slot.borrow_mut() = Some(Rc::new(UiWindows {
            overlay: overlay.window.clone(),
            toolbar: toolbar.window.clone(),
        }));
    });

    if !composited {
        let dialog = gtk::MessageDialog::new(
            Some(&toolbar.window),
            gtk::DialogFlags::empty(),
            gtk::MessageType::Warning,
            gtk::ButtonsType::Ok,
            "没有检测到合成器，透明覆盖层可能显示为黑色或无法穿透。",
        );
        dialog.set_title("LInk");
        dialog.connect_response(|dialog, _| dialog.close());
        dialog.show_all();
    }

    log::info!("LInk 就绪");

    Ok(())
}

fn install_region_reapply(
    platform: &Rc<Platform>,
    overlay: &gtk::Window,
    area: &gtk::DrawingArea,
    passing: Rc<Cell<bool>>,
) {
    let platform_re = platform.clone();
    let overlay_re = overlay.clone();
    let area_re = area.clone();
    let passing_re = passing.clone();
    overlay.connect_map(move |_| {
        if passing_re.get() {
            reapply(&platform_re, &overlay_re, &area_re);
        }
    });

    let platform_re = platform.clone();
    let overlay_re = overlay.clone();
    let area_re = area.clone();
    let passing_re = passing;
    area.connect_size_allocate(move |_, _| {
        if passing_re.get() {
            reapply(&platform_re, &overlay_re, &area_re);
        }
    });
}

fn reapply(platform: &Platform, overlay: &gtk::Window, area: &gtk::DrawingArea) {
    if let Err(err) = platform.reapply_input_region(&input_windows(overlay, area)) {
        log::warn!("重新应用穿透输入区域失败: {err}");
    }
}

fn input_windows(overlay: &gtk::Window, area: &gtk::DrawingArea) -> Vec<gdk::Window> {
    let mut windows = Vec::new();
    if let Some(window) = overlay.window() {
        windows.push(window);
    }
    if let Some(window) = area.window() {
        windows.push(window);
    }
    windows
}

fn log_window_geometry_once(platform: Rc<Platform>, monitor: MonitorInfo, overlay: gtk::Window) {
    let logged = Rc::new(Cell::new(false));
    overlay.connect_map(move |window| {
        if logged.replace(true) {
            return;
        }
        let Some(gdk_window) = window.window() else {
            return;
        };
        match platform.window_geometry(&gdk_window) {
            Ok(geometry) => log::info!(
                "覆盖层窗口 {}x{}+{}+{}（期望物理 {}x{}+{}+{}）",
                geometry.width,
                geometry.height,
                geometry.x,
                geometry.y,
                monitor.physical.width,
                monitor.physical.height,
                monitor.physical.x,
                monitor.physical.y
            ),
            Err(err) => log::warn!("覆盖层几何查询失败: {err}"),
        }
    });
}

fn show_error(parent: Option<&gtk::Window>, title: &str, message: &str) {
    let dialog = gtk::MessageDialog::new(
        parent,
        gtk::DialogFlags::empty(),
        gtk::MessageType::Error,
        gtk::ButtonsType::Ok,
        message,
    );
    dialog.set_title(title);
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.show_all();
}

#[cfg(test)]
mod damage_tests {
    use super::{integer_rect, union_rect};

    #[test]
    fn union_covers_both_frames() {
        let a = Some((10.0, 10.0, 20.0, 20.0));
        let b = Some((15.0, 5.0, 30.0, 12.0));
        assert_eq!(union_rect(a, b), Some((10.0, 5.0, 30.0, 20.0)));
    }

    #[test]
    fn union_keeps_single_side_when_other_missing() {
        let a = Some((1.0, 2.0, 3.0, 4.0));
        assert_eq!(union_rect(a, None), a);
        assert_eq!(union_rect(None, a), a);
        assert_eq!(union_rect(None, None), None);
    }

    #[test]
    fn integer_rect_expands_outward_for_antialiasing() {
        let (x, y, w, h) = integer_rect((10.4, 20.6, 30.2, 40.9));
        assert_eq!((x, y), (9, 19));
        assert_eq!((x + w, y + h), (32, 42));
    }
}
