mod common;

use common::TestTarget;
use link_ink::model::{Document, Point, Rgba, Stroke};
use link_ink::platform::{MonitorInfo, RectF, RectI};
use link_ink::render::{draw_document, draw_marquee, draw_selection, draw_selection_box};
use link_ink::selection::{MarqueeKind, SelectionTransform};
use link_ink::view::ViewTransform;

fn view_transform() -> (ViewTransform, Document) {
    let monitor = MonitorInfo {
        physical: RectI {
            x: 0,
            y: 0,
            width: 1000,
            height: 1000,
        },
        logical: RectF {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 1000.0,
        },
        scale: 1.0,
    };
    let document = Document::new(monitor.width_pt(), monitor.height_pt());
    let view = ViewTransform::for_monitor(&monitor, document.width_pt, document.height_pt);
    (view, document)
}

fn render(document: &Document, view: &ViewTransform) -> TestTarget {
    let mut target = TestTarget::new(1000, 1000);
    target.canvas(|canvas| draw_document(canvas, document, view));
    target
}

#[test]
fn constant_stroke_maps_page_to_logical() {
    let (view, mut document) = view_transform();
    let stroke = Stroke::constant(
        Rgba::BLACK,
        10.0,
        vec![Point::new(75.0, 75.0), Point::new(225.0, 75.0)],
    );
    document.layers[0].strokes.push(stroke);

    let target = render(&document, &view);
    assert!(target.alpha_at(200, 100) > 200);
    assert!(target.alpha_at(100, 100) > 100);
    assert_eq!(target.alpha_at(200, 200), 0);
    assert_eq!(target.alpha_at(600, 600), 0);
}

#[test]
fn dot_stroke_is_round() {
    let (view, mut document) = view_transform();
    document.layers[0].strokes.push(Stroke::constant(
        Rgba::BLACK,
        20.0,
        vec![Point::new(375.0, 375.0)],
    ));

    let target = render(&document, &view);
    assert!(target.alpha_at(500, 500) > 200);
    assert!(target.alpha_at(500, 510) > 200);
    // 半径 10pt + 抗锯齿：对角线方向 12pt 处应已透明
    assert_eq!(target.alpha_at(500, 526), 0);
}

#[test]
fn round_cap_extends_beyond_endpoint() {
    let (view, mut document) = view_transform();
    document.layers[0].strokes.push(Stroke::constant(
        Rgba::BLACK,
        16.0,
        vec![Point::new(75.0, 375.0), Point::new(225.0, 375.0)],
    ));

    let target = render(&document, &view);
    assert!(target.alpha_at(100, 500) > 200);
    assert!(target.alpha_at(95, 500) > 200);
    assert_eq!(target.alpha_at(80, 500), 0);
}

#[test]
fn variable_width_tapers() {
    let (view, mut document) = view_transform();
    document.layers[0].strokes.push(Stroke::variable(
        Rgba::BLACK,
        30.0,
        vec![2.0, 30.0],
        vec![Point::new(75.0, 375.0), Point::new(225.0, 375.0)],
    ));

    let target = render(&document, &view);
    assert!(target.alpha_at(110, 500) > 200);
    assert_eq!(target.alpha_at(110, 516), 0);
    assert!(target.alpha_at(295, 500) > 200);
    assert!(target.alpha_at(295, 516) > 200);
}

#[test]
fn selection_uses_dashed_edges() {
    let (view, _) = view_transform();
    let stroke = Stroke::constant(
        Rgba::BLACK,
        10.0,
        vec![Point::new(75.0, 75.0), Point::new(225.0, 75.0)],
    );
    let mut target = TestTarget::new(1000, 1000);
    target.canvas(|canvas| {
        canvas.clip_rect(0.0, 0.0, 1000.0, 1000.0);
        draw_selection(canvas, &[&stroke], &view, SelectionTransform::None);
    });
    let mut contrast = 0;
    for y in 88..98 {
        let values: Vec<u8> = (130..290).map(|x| target.alpha_at(x, y)).collect();
        let min = *values.iter().min().unwrap();
        let max = *values.iter().max().unwrap();
        contrast = contrast.max(max - min);
    }
    assert!(contrast > 100, "笔迹边缘应呈现明暗交替的虚线描边");
}

#[test]
fn selection_box_draws_thin_solid_border_and_handles() {
    let (view, _) = view_transform();
    let mut target = TestTarget::new(1000, 1000);
    target.canvas(|canvas| {
        canvas.clip_rect(0.0, 0.0, 1000.0, 1000.0);
        draw_selection_box(canvas, (75.0, 75.0, 225.0, 225.0), SelectionTransform::None, &view);
    });
    assert!(target.alpha_at(150, 100) > 0, "选框上边应为细实线");
    assert_eq!(target.alpha_at(200, 200), 0, "选框内部应为空");
    assert!(target.alpha_at(100, 100) > 0, "角手柄应可见");
    assert!(target.alpha_at(200, 100) > 0, "边中手柄应可见");
}

#[test]
fn marquee_rect_draws_a_visible_border() {
    let (view, _) = view_transform();
    let mut target = TestTarget::new(1000, 1000);
    target.canvas(|canvas| {
        canvas.clip_rect(0.0, 0.0, 1000.0, 1000.0);
        draw_marquee(
            canvas,
            MarqueeKind::Rect,
            Point::new(75.0, 75.0),
            Point::new(225.0, 225.0),
            &[],
            &view,
        );
    });
    assert!(target.alpha_at(150, 100) > 0, "选框上边应可见");
    assert!(target.alpha_at(100, 150) > 0, "选框左边应可见");
    assert!(target.alpha_at(200, 200) > 0, "选框内部应有淡色填充");
    assert_eq!(target.alpha_at(600, 600), 0);
}

#[test]
fn layer_order_is_bottom_to_top() {
    let (view, mut document) = view_transform();
    document.layers[0].strokes.push(Stroke::constant(
        Rgba::new(255, 0, 0, 255),
        20.0,
        vec![Point::new(150.0, 150.0), Point::new(600.0, 150.0)],
    ));
    document.add_layer_above_active();
    document.layers[1].strokes.push(Stroke::constant(
        Rgba::new(0, 0, 255, 255),
        20.0,
        vec![Point::new(150.0, 150.0), Point::new(600.0, 150.0)],
    ));

    let target = render(&document, &view);
    let pixel = target.pixel_at(400, 200);
    assert!(
        pixel[2] > 200 && pixel[0] < 50,
        "顶层蓝色应盖住底层红色，实际 {pixel:?}"
    );
}
