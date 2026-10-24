//! 诊断工具：对比「逐帧重绘全部笔迹」与「离屏缓存 + 脏矩形」的成本。
//!
//! cargo run --release --example probe_perf

use std::time::Instant;

use cairo::{Context, Format, ImageSurface};
use link_ink::cache::OverlayCache;
use link_ink::model::{Document, Point, Rgba, Stroke};
use link_ink::platform::{MonitorInfo, RectF, RectI};
use link_ink::render::draw_document;
use link_ink::view::ViewTransform;

const W: i32 = 4384;
const H: i32 = 2466;

fn monitor() -> MonitorInfo {
    MonitorInfo {
        physical: RectI {
            x: 0,
            y: 0,
            width: W as u32,
            height: H as u32,
        },
        logical: RectF {
            x: 0.0,
            y: 0.0,
            width: 2192.0,
            height: 1233.0,
        },
        scale: 2.0,
    }
}

fn build(count: usize) -> Document {
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
    doc
}

fn surface() -> ImageSurface {
    let surface = ImageSurface::create(Format::ARgb32, W, H).unwrap();
    surface.set_device_scale(2.0, 2.0);
    surface
}

fn main() {
    let monitor = monitor();
    let view = ViewTransform::for_monitor(&monitor, 1644.0, 924.75);
    println!("画布 {W}x{H} @2x\n");
    println!(
        "{:>7} | {:>12} | {:>12} | {:>12} | {:>10}",
        "笔数", "旧:整屏重绘", "新:整屏贴图", "新:脏矩形帧", "缓存重建"
    );

    for count in [100usize, 1000, 3000] {
        let doc = build(count);

        // 旧路径：清屏 + 重画全部笔迹
        let target = surface();
        let full = {
            let cr = Context::new(&target).unwrap();
            let start = Instant::now();
            cr.set_operator(cairo::Operator::Source);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
            cr.paint().unwrap();
            draw_document(&cr, &doc, &view).unwrap();
            drop(cr);
            target.flush();
            start.elapsed()
        };

        // 新路径：整屏贴缓存
        let mut cache = OverlayCache::new(&monitor, 2.0);
        cache.sync(&doc, &view);
        let blit_full = {
            let cr = Context::new(&target).unwrap();
            let start = Instant::now();
            cache.paint(&cr).unwrap();
            drop(cr);
            target.flush();
            start.elapsed()
        };

        // 新路径：真实的一帧 = sync（通常无变化）+ 只贴笔尖附近 200x120 逻辑像素
        let frames = 120;
        let start = Instant::now();
        for frame in 0..frames {
            let cr = Context::new(&target).unwrap();
            cr.rectangle(
                300.0 + (frame % 40) as f64,
                200.0 + (frame % 20) as f64,
                200.0,
                120.0,
            );
            cr.clip();
            cache.sync(&doc, &view);
            cache.paint(&cr).unwrap();
            drop(cr);
            target.flush();
        }
        let frame_cost = start.elapsed() / frames;

        // 缓存重建（擦除/切层时才会发生）
        let mut fresh = OverlayCache::new(&monitor, 2.0);
        let start = Instant::now();
        fresh.sync(&doc, &view);
        let rebuild = start.elapsed();

        println!(
            "{count:>7} | {:>10.2}ms | {:>10.2}ms | {:>10.2}ms | {:>8.2}ms",
            ms(full),
            ms(blit_full),
            ms(frame_cost),
            ms(rebuild)
        );
    }
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
