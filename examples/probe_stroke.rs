//! 临时诊断工具：放大渲染可疑几何，检查变宽笔迹轮廓是否出现尖角凸起。
//!
//! cargo run --example probe_stroke -- target/verify

use std::path::PathBuf;

use link_ink::model::{Point, Rgba, Stroke};
use link_ink::paint::{Cap, Color, Join, PathBuilder2, Surface};
use link_ink::platform::{MonitorInfo, RectF, RectI};
use link_ink::render::draw_stroke;
use link_ink::view::ViewTransform;

const ZOOM: f64 = 6.0;

fn monitor() -> MonitorInfo {
    MonitorInfo {
        physical: RectI {
            x: 0,
            y: 0,
            width: 800,
            height: 400,
        },
        logical: RectF {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 400.0,
        },
        scale: 1.0,
    }
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).unwrap();
    let monitor = monitor();

    let corner = vec![
        Point::new(40.0, 40.0),
        Point::new(160.0, 40.0),
        Point::new(160.0, 140.0),
        Point::new(40.0, 140.0),
    ];

    save_reference(&dir.join("probe_ref_native.png"), &corner, 2.0, &monitor);
    save(
        &dir.join("probe_corner_constant.png"),
        &Stroke::constant(Rgba::BLACK, 2.0, corner.clone()),
        &monitor,
    );
    save(
        &dir.join("probe_corner_variable.png"),
        &Stroke::variable(Rgba::BLACK, 2.0, vec![1.0, 5.0, 1.0, 5.0], corner.clone()),
        &monitor,
    );

    let mut dense = Vec::new();
    for i in 0..=60 {
        dense.push(Point::new(40.0 + i as f64 * 2.0, 40.0));
    }
    for i in 1..=50 {
        dense.push(Point::new(160.0, 40.0 + i as f64 * 2.0));
    }
    save(
        &dir.join("probe_dense_corner.png"),
        &Stroke::variable(Rgba::BLACK, 2.0, vec![3.0; dense.len()], dense),
        &monitor,
    );

    let mut hand = Vec::new();
    let mut hand_w = Vec::new();
    for i in 0..=80 {
        let t = i as f64 / 80.0;
        hand.push(Point::new(
            40.0 + t * 240.0,
            120.0 + (t * 9.0).sin() * 40.0 * (1.0 - t),
        ));
        hand_w.push(1.0 + 4.0 * (t * 6.0).sin().abs());
    }
    save(
        &dir.join("probe_handwriting.png"),
        &Stroke::variable(Rgba::BLACK, 2.0, hand_w, hand),
        &monitor,
    );

    // 6. 快速书写：采样点稀疏（每点间隔 20~40pt），模拟笔快速划过
    let sparse_points = vec![
        Point::new(40.0, 60.0),
        Point::new(75.0, 95.0),
        Point::new(115.0, 45.0),
        Point::new(150.0, 90.0),
        Point::new(190.0, 50.0),
        Point::new(230.0, 85.0),
        Point::new(270.0, 55.0),
    ];
    let sparse_widths = vec![2.0, 3.0, 2.2, 3.2, 2.0, 2.8, 2.4];
    save(
        &dir.join("probe_fast_sparse.png"),
        &Stroke::variable(Rgba::BLACK, 3.7, sparse_widths, sparse_points),
        &monitor,
    );

    // 7. 稀疏采样 + 尖锐折角（检查过冲）
    let corner_points = vec![
        Point::new(40.0, 160.0),
        Point::new(120.0, 160.0),
        Point::new(120.0, 220.0),
        Point::new(200.0, 160.0),
    ];
    save(
        &dir.join("probe_fast_corner.png"),
        &Stroke::variable(Rgba::BLACK, 3.7, vec![2.5; 4], corner_points),
        &monitor,
    );

    println!("probes written to {}", dir.display());
}

fn view(monitor: &MonitorInfo) -> ViewTransform {
    ViewTransform::for_monitor(monitor, monitor.width_pt(), monitor.height_pt())
        .to_physical_pixels(ZOOM)
}

fn canvas(monitor: &MonitorInfo) -> Box<dyn Surface> {
    let width = (monitor.logical.width * ZOOM) as u32;
    let height = (monitor.logical.height * ZOOM) as u32;
    let mut surface = link_ink::paint::new_surface(width, height).unwrap();
    surface.fill(Color::WHITE);
    surface
}

fn save(path: &PathBuf, stroke: &Stroke, monitor: &MonitorInfo) {
    let mut surface = canvas(monitor);
    let view = view(monitor);
    surface.with_canvas(&mut |canvas| draw_stroke(canvas, stroke, &view));
    write_png(surface.as_mut(), path);
}

/// 不经过平滑/变宽轮廓，直接用后端描一条折线，作为“原始几何”的参照。
fn save_reference(path: &PathBuf, points: &[Point], width_pt: f64, monitor: &MonitorInfo) {
    let mut surface = canvas(monitor);
    let view = view(monitor);
    surface.with_canvas(&mut |canvas| {
        let (origin_x, origin_y) = view.origin_logical();
        canvas.translate(origin_x, origin_y);
        canvas.scale(view.pt_to_logical(), view.pt_to_logical());
        canvas.set_line_cap(Cap::Round);
        canvas.set_line_join(Join::Round);
        canvas.set_line_width(width_pt);
        canvas.set_color(Color::new(0, 0, 0, 255));
        let mut builder = PathBuilder2::new();
        builder.move_to(points[0].x, points[0].y);
        for point in &points[1..] {
            builder.line_to(point.x, point.y);
        }
        if let Some(path) = builder.finish() {
            canvas.stroke_path(&path);
        }
    });
    write_png(surface.as_mut(), path);
}

fn write_png(surface: &mut dyn Surface, path: &PathBuf) {
    let width = surface.width();
    let height = surface.height();
    let rgba = surface.read_rgba();
    image::save_buffer(
        path,
        &rgba,
        width,
        height,
        image::ColorType::Rgba8,
    )
    .unwrap();
}
