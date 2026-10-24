//! 诊断：整帧合成（缓存 + 草稿，经 cairo group）在不同裁剪区域下的成本。
use std::time::Instant;

use cairo::{Context, Format, ImageSurface};
use link_ink::app::App;
use link_ink::cache::OverlayCache;
use link_ink::input::{InputEvent, PointerSample, ToolKind};
use link_ink::model::{Document, Point, Rgba, Stroke};
use link_ink::platform::{MonitorInfo, RectF, RectI};
use link_ink::view::ViewTransform;

fn main() {
    let monitor = MonitorInfo {
        physical: RectI {
            x: 0,
            y: 0,
            width: 4384,
            height: 2466,
        },
        logical: RectF {
            x: 0.0,
            y: 0.0,
            width: 2192.0,
            height: 1233.0,
        },
        scale: 2.0,
    };
    let view = ViewTransform::for_monitor(&monitor, 1644.0, 924.75);

    for count in [100usize, 1000, 3000] {
        let mut doc = Document::new(1644.0, 924.75);
        for i in 0..count {
            let x = 40.0 + (i % 40) as f64 * 40.0;
            let y = 40.0 + (i / 40) as f64 * 18.0;
            let pts: Vec<Point> = (0..24)
                .map(|k| Point::new(x + k as f64 * 1.4, y + (k as f64 * 0.6).sin() * 4.0))
                .collect();
            let widths: Vec<f64> = (0..24).map(|k| 0.8 + 2.0 * (k as f64 / 23.0)).collect();
            doc.active_layer_mut()
                .strokes
                .push(Stroke::variable(Rgba::BLACK, 3.7, widths, pts));
        }

        let mut app = App::new(doc);
        // 一条 900 点的长草稿
        let sample = |x: f64, y: f64| PointerSample {
            logical: (x, y),
            pressure: 0.6,
            pressure_available: true,
            device_has_pressure: true,
            tool: ToolKind::Pen,
            device_name: "bench".to_string(),
            tilt: None,
            shift: false,
        };
        app.handle_input(InputEvent::Down(sample(100.0, 300.0)), &view);
        for i in 1..=900 {
            let t = i as f64 / 900.0;
            app.handle_input(
                InputEvent::Move(sample(100.0 + t * 1800.0, 300.0 + (t * 18.0).sin() * 200.0)),
                &view,
            );
        }

        let mut cache = OverlayCache::new(&monitor, 2.0);
        cache.sync(&app.document, &view);
        let target = ImageSurface::create(Format::ARgb32, 4384, 2466).unwrap();
        target.set_device_scale(2.0, 2.0);

        let small = frame_cost(
            &target,
            &app,
            &view,
            &mut cache,
            Some((900.0, 250.0, 200.0, 120.0)),
        );
        let full = frame_cost(&target, &app, &view, &mut cache, None);
        println!(
            "{count:>5} 笔 | 脏矩形帧 {:5.2}ms | 整屏帧 {:6.2}ms",
            small, full
        );
    }
}

fn frame_cost(
    target: &ImageSurface,
    app: &App,
    view: &ViewTransform,
    cache: &mut OverlayCache,
    clip: Option<(f64, f64, f64, f64)>,
) -> f64 {
    let frames = if clip.is_some() { 60 } else { 5 };
    let start = Instant::now();
    for _ in 0..frames {
        let cr = Context::new(target).unwrap();
        if let Some((x, y, w, h)) = clip {
            cr.rectangle(x, y, w, h);
            cr.clip();
        }
        link_ink::ui::overlay::draw(&cr, app, view, cache);
        drop(cr);
        target.flush();
    }
    start.elapsed().as_secs_f64() * 1000.0 / frames as f64
}
