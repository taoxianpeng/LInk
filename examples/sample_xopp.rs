//! 生成一份用于 Xournal++ 兼容性验收的样本工程。
//!
//! 用法：
//! ```text
//! cargo run --example sample_xopp -- /tmp/link_sample.xopp
//! gzip -dc /tmp/link_sample.xopp | head -40
//! xournalpp -i /tmp/link_sample.png /tmp/link_sample.xopp
//! ```
//!
//! 样本覆盖：多图层顺序、常量宽笔迹、压感变宽笔迹、半透明颜色、
//! 以及转换为笔迹保存的直线/矩形/椭圆。

use std::path::PathBuf;

use link_ink::model::{Document, Point, Rgba, Stroke};
use link_ink::xopp;

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("link_sample.xopp"));

    let mut document = Document::new(1644.0, 924.75);

    // 底层：常量宽黑笔 + 一条用压力曲线生成的变宽笔迹。
    document.layers[0].name = "底层".to_string();
    document.layers[0].strokes.push(Stroke::constant(
        Rgba::new(0, 0, 0, 255),
        2.0,
        vec![
            Point::new(80.0, 80.0),
            Point::new(300.0, 120.0),
            Point::new(520.0, 90.0),
        ],
    ));
    document.layers[0].strokes.push(Stroke::variable(
        Rgba::new(200, 30, 30, 255),
        8.0,
        vec![0.6, 2.4, 6.0, 9.0, 5.0, 1.2],
        vec![
            Point::new(80.0, 200.0),
            Point::new(160.0, 175.0),
            Point::new(240.0, 215.0),
            Point::new(320.0, 180.0),
            Point::new(400.0, 210.0),
            Point::new(480.0, 190.0),
        ],
    ));

    // 上层：蓝绿半透明，验证图层顺序与 alpha。
    document.add_layer_above_active();
    document.layers[1].name = "上层".to_string();
    document.layers[1]
        .strokes
        .push(Stroke::constant(Rgba::new(0, 120, 255, 128), 6.0, line()));
    document.layers[1]
        .strokes
        .push(Stroke::constant(Rgba::new(0, 160, 80, 255), 2.0, rect()));
    document.layers[1].strokes.push(Stroke::constant(
        Rgba::new(120, 0, 200, 255),
        2.0,
        ellipse(),
    ));

    match xopp::save(&path, &document) {
        Ok(()) => println!(
            "已写入 {}（{} 图层，{} 笔迹）",
            path.display(),
            document.layers.len(),
            document
                .layers
                .iter()
                .map(|layer| layer.strokes.len())
                .sum::<usize>()
        ),
        Err(error) => {
            eprintln!("写入失败: {error}");
            std::process::exit(1);
        }
    }
}

fn line() -> Vec<Point> {
    vec![Point::new(700.0, 120.0), Point::new(1300.0, 300.0)]
}

fn rect() -> Vec<Point> {
    vec![
        Point::new(760.0, 400.0),
        Point::new(1240.0, 400.0),
        Point::new(1240.0, 640.0),
        Point::new(760.0, 640.0),
        Point::new(760.0, 400.0),
    ]
}

fn ellipse() -> Vec<Point> {
    let (cx, cy) = (400.0, 520.0);
    let (rx, ry) = (220.0, 120.0);
    let segments = 96;
    (0..=segments)
        .map(|index| {
            let angle = index as f64 / segments as f64 * std::f64::consts::TAU;
            Point::new(cx + rx * angle.cos(), cy + ry * angle.sin())
        })
        .collect()
}
