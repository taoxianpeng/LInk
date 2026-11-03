//! 诊断：整帧合成（缓存 + 草稿，经离屏组）在不同裁剪区域下的成本。
//!
//! 两个平台跑同一份绘制抽象：Linux 走 cairo，Windows 走 tiny-skia。
use std::time::Instant;

use link_ink::app::App;
use link_ink::cache::OverlayCache;
use link_ink::input::{InputEvent, PointerSample, ToolKind};
use link_ink::model::{Document, Point, Rgba, Stroke};
use link_ink::paint::RectF;
use link_ink::platform::{MonitorInfo, RectF as LogicalRect, RectI};
use link_ink::view::ViewTransform;

fn monitor() -> MonitorInfo {
    MonitorInfo {
        physical: RectI {
            x: 0,
            y: 0,
            width: 4384,
            height: 2466,
        },
        logical: LogicalRect {
            x: 0.0,
            y: 0.0,
            width: 2192.0,
            height: 1233.0,
        },
        scale: 2.0,
    }
}

fn sample(x: f64, y: f64) -> PointerSample {
    PointerSample {
        logical: (x, y),
        pressure: 0.7,
        pressure_available: true,
        device_has_pressure: true,
        tool: ToolKind::Pen,
        device_name: "bench".to_string(),
        tilt: None,
        shift: false,
    }
}

fn main() {
    let monitor = monitor();
    let logical_view = ViewTransform::for_monitor(&monitor, 1644.0, 924.75);
    let physical = logical_view.to_physical_pixels(2.0);

    for count in [100usize, 1000, 3000] {
        let mut doc = Document::new(1644.0, 924.75);
        for index in 0..count {
            let y = 40.0 + (index % 14) as f64 * 60.0;
            let x = 30.0 + (index / 14) as f64 * 26.0;
            doc.layers[0].strokes.push(Stroke::variable(
                Rgba::BLACK,
                3.7,
                vec![1.5, 3.0, 2.2, 3.4],
                vec![
                    Point::new(x, y),
                    Point::new(x + 30.0, y + 8.0),
                    Point::new(x + 60.0, y - 6.0),
                    Point::new(x + 90.0, y + 4.0),
                ],
            ));
        }
        let mut app = App::new(doc);
        app.handle_input(InputEvent::Down(sample(100.0, 300.0)), &logical_view);
        for i in 1..=900 {
            let t = i as f64 / 900.0;
            app.handle_input(
                InputEvent::Move(sample(
                    100.0 + t * 1800.0,
                    300.0 + (t * 18.0).sin() * 200.0,
                )),
                &logical_view,
            );
        }

        let mut cache = OverlayCache::new(&monitor, 2.0);
        cache.sync(&app.document, &physical);

        let small = frame_cost(&mut app, &physical, &mut cache, Some((900.0, 250.0, 200.0, 120.0)));
        let full = frame_cost(&mut app, &physical, &mut cache, None);
        println!(
            "{count:>5} 笔 | 脏矩形帧 {:5.2}ms | 整屏帧 {:6.2}ms",
            small, full
        );
    }
}

fn frame_cost(
    app: &mut App,
    view: &ViewTransform,
    cache: &mut OverlayCache,
    clip: Option<(f64, f64, f64, f64)>,
) -> f64 {
    let frames = if clip.is_some() { 60 } else { 5 };
    let width = 4384u32;
    let height = 2466u32;
    let mut target = link_ink::paint::new_surface(width, height).unwrap();
    let start = Instant::now();
    for _ in 0..frames {
        target.with_canvas(&mut |canvas| {
            let bounds = match clip {
                Some((x, y, w, h)) => {
                    canvas.clip_rect(x, y, w, h);
                    RectF::new(x, y, w, h)
                }
                None => RectF::new(0.0, 0.0, width as f64, height as f64),
            };
            link_ink::ui::overlay::draw(canvas, app, view, cache, None, bounds);
        });
    }
    start.elapsed().as_secs_f64() * 1000.0 / frames as f64
}
