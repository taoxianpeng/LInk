//! 诊断工具：对比「逐帧重绘全部笔迹」与「离屏缓存 + 脏矩形」的成本。
//!
//! cargo run --release --example probe_perf
//!
//! 两个平台都跑同一份绘制抽象：Linux 走 cairo，Windows 走 tiny-skia。

use std::time::Instant;

use link_ink::cache::OverlayCache;
use link_ink::model::{Document, Point, Rgba, Stroke};
use link_ink::platform::{MonitorInfo, RectF, RectI};
use link_ink::render;
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
            width: (W / 2) as f64,
            height: (H / 2) as f64,
        },
        scale: 2.0,
    }
}

fn build(count: usize) -> Document {
    let mut doc = Document::new(1644.0, 924.75);
    let mut x = 20.0f64;
    let mut y = 40.0f64;
    for index in 0..count {
        let pts = (0..24)
            .map(|step| {
                Point::new(
                    x + step as f64 * 12.0,
                    y + (step as f64 * 0.7 + index as f64).sin() * 18.0,
                )
            })
            .collect::<Vec<_>>();
        let widths = (0..24)
            .map(|step| 1.2 + (step as f64 * 0.3).sin().abs() * 3.0)
            .collect::<Vec<_>>();
        doc.layers[0]
            .strokes
            .push(Stroke::variable(Rgba::BLACK, 3.7, widths, pts));
        x += 42.0;
        if x > 1400.0 {
            x = 20.0;
            y += 60.0;
            if y > 880.0 {
                y = 40.0;
            }
        }
    }
    doc
}

/// 物理像素视图（Canvas 抽象统一用设备像素）。
fn physical_view() -> ViewTransform {
    ViewTransform::for_monitor(&monitor(), 1644.0, 924.75).to_physical_pixels(2.0)
}

fn main() {
    let monitor = monitor();
    let view = physical_view();
    println!("画布 {W}x{H} @2x（物理像素视图）\n");
    println!(
        "{:>7} | {:>12} | {:>12} | {:>12} | {:>10}",
        "笔数", "旧:整屏重绘", "新:整屏贴图", "新:脏矩形帧", "缓存重建"
    );

    for count in [100usize, 1000, 3000] {
        let doc = build(count);

        let mut target = link_ink::paint::new_surface(W as u32, H as u32).unwrap();

        // 旧路径：清屏 + 重画全部笔迹
        let full = {
            let start = Instant::now();
            target.with_canvas(&mut |canvas| {
                canvas.paint_with(
                    link_ink::paint::Color::TRANSPARENT,
                    link_ink::paint::Blend::Source,
                );
                render::draw_document(canvas, &doc, &view);
            });
            start.elapsed()
        };

        // 新路径：整屏贴缓存
        let mut cache = OverlayCache::new(&monitor, 2.0);
        cache.sync(&doc, &view);
        let blit_full = {
            let start = Instant::now();
            target.with_canvas(&mut |canvas| cache.paint(canvas));
            start.elapsed()
        };

        // 新路径：真实的一帧 = sync（通常无变化）+ 只贴笔尖附近 200x120 逻辑像素
        let frames = 120;
        let start = Instant::now();
        for frame in 0..frames {
            target.with_canvas(&mut |canvas| {
                canvas.clip_rect(
                    300.0 + (frame % 40) as f64,
                    200.0 + (frame % 20) as f64,
                    200.0,
                    120.0,
                );
                cache.sync(&doc, &view);
                cache.paint(canvas);
            });
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
