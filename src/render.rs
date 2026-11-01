use std::f64::consts::PI;

use cairo::{Context, LineCap, LineJoin};

use crate::model::{Document, Layer, Point, Stroke};
use crate::selection::{MarqueeKind, Rect, SelectionTransform};
use crate::view::ViewTransform;

const SELECTION_COLOR: (f64, f64, f64, f64) = (0.31765, 0.27843, 0.89804, 1.0);
const SELECTION_HALO: (f64, f64, f64, f64) = (1.0, 1.0, 1.0, 0.92);

const CAP_SEGMENTS: usize = 10;
/// 平滑插值的最大步长（页面 pt）。约等于 2 个逻辑像素，足够消除折痕。
const SMOOTH_MAX_STEP_PT: f64 = 1.5;
/// 平滑后的点数上限，避免极长笔迹把每帧成本拉爆。
const SMOOTH_MAX_POINTS: usize = 24_000;

pub fn draw_document(cr: &Context, document: &Document, view: &ViewTransform) -> cairo::Result<()> {
    cr.save()?;
    apply_transform(cr, view)?;
    cr.set_line_cap(LineCap::Round);
    cr.set_line_join(LineJoin::Round);
    for layer in &document.layers {
        if !layer.visible {
            continue;
        }
        for stroke in &layer.strokes {
            paint_stroke(cr, stroke)?;
        }
    }
    cr.restore()
}

/// 只画 `layer_index` **之上**的那些图层的笔迹。
///
/// 草稿（正在画的那一笔）属于当前图层，必须夹在"它下面的层"和"它上面的层"之间；
/// 否则在非顶层书写时，笔迹会浮在所有图层之上，等到提交（重新合成缓存）才跳回正确位置。
///
/// `cull_pt` 是本帧要重绘的页面区域，包围盒不相交的笔迹直接跳过 —— 这条路径每帧都会走，
/// 上层笔迹多时靠它把成本压在一小块区域内。
pub fn draw_layers_above(
    cr: &Context,
    document: &Document,
    layer_index: usize,
    view: &ViewTransform,
    cull_pt: Option<(f64, f64, f64, f64)>,
) -> cairo::Result<()> {
    let above = document.layers.len().saturating_sub(layer_index + 1);
    if above == 0 {
        return Ok(());
    }
    cr.save()?;
    apply_transform(cr, view)?;
    cr.set_line_cap(LineCap::Round);
    cr.set_line_join(LineJoin::Round);
    for layer in &document.layers[layer_index + 1..] {
        if !layer.visible {
            continue;
        }
        for stroke in &layer.strokes {
            if let Some(cull) = cull_pt {
                if !rects_overlap(stroke.bounds_pt(), cull) {
                    continue;
                }
            }
            paint_stroke(cr, stroke)?;
        }
    }
    cr.restore()
}

fn rects_overlap(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
    a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
}

pub fn draw_stroke(cr: &Context, stroke: &Stroke, view: &ViewTransform) -> cairo::Result<()> {
    cr.save()?;
    apply_transform(cr, view)?;
    cr.set_line_cap(LineCap::Round);
    cr.set_line_join(LineJoin::Round);
    paint_stroke(cr, stroke)?;
    cr.restore()
}

pub fn draw_layer(cr: &Context, layer: &Layer, view: &ViewTransform) -> cairo::Result<()> {
    cr.save()?;
    apply_transform(cr, view)?;
    cr.set_line_cap(LineCap::Round);
    cr.set_line_join(LineJoin::Round);
    for stroke in &layer.strokes {
        paint_stroke(cr, stroke)?;
    }
    cr.restore()
}

pub fn draw_selection(
    cr: &Context,
    strokes: &[&Stroke],
    view: &ViewTransform,
    transform: SelectionTransform,
) -> cairo::Result<()> {
    cr.save()?;
    apply_transform(cr, view)?;
    apply_selection_transform(cr, transform);
    let scale = view.pt_to_logical().max(1e-6);
    cr.set_dash(&[5.0 / scale, 4.0 / scale], 0.0);
    cr.set_line_cap(LineCap::Round);
    cr.set_line_join(LineJoin::Round);
    for stroke in strokes {
        if !build_selection_edge_path(cr, stroke) {
            continue;
        }
        cr.set_source_rgba(SELECTION_HALO.0, SELECTION_HALO.1, SELECTION_HALO.2, 0.85);
        cr.set_line_width(2.4 / scale);
        cr.stroke_preserve()?;
        cr.set_source_rgba(
            SELECTION_COLOR.0,
            SELECTION_COLOR.1,
            SELECTION_COLOR.2,
            0.95,
        );
        cr.set_line_width(1.3 / scale);
        cr.stroke()?;
    }
    cr.set_dash(&[], 0.0);
    cr.restore()
}

pub fn draw_selection_box(
    cr: &Context,
    bounds: Rect,
    transform: SelectionTransform,
    view: &ViewTransform,
) -> cairo::Result<()> {
    cr.save()?;
    apply_transform(cr, view)?;
    apply_selection_transform(cr, transform);
    let scale = view.pt_to_logical().max(1e-6);
    let corners = [
        Point::new(bounds.0, bounds.1),
        Point::new(bounds.2, bounds.1),
        Point::new(bounds.2, bounds.3),
        Point::new(bounds.0, bounds.3),
    ];
    cr.set_source_rgba(
        SELECTION_COLOR.0,
        SELECTION_COLOR.1,
        SELECTION_COLOR.2,
        0.95,
    );
    cr.set_line_width(1.0 / scale);
    cr.new_path();
    cr.move_to(corners[0].x, corners[0].y);
    for point in &corners[1..] {
        cr.line_to(point.x, point.y);
    }
    cr.close_path();
    cr.stroke()?;

    let half = 3.0 / scale;
    let mut handles = Vec::with_capacity(8);
    handles.extend_from_slice(&corners);
    for index in 0..4 {
        let a = corners[index];
        let b = corners[(index + 1) % 4];
        handles.push(Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0));
    }
    for point in handles {
        cr.rectangle(point.x - half, point.y - half, half * 2.0, half * 2.0);
        cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
        cr.fill_preserve()?;
        cr.set_source_rgba(SELECTION_COLOR.0, SELECTION_COLOR.1, SELECTION_COLOR.2, 1.0);
        cr.set_line_width(1.0 / scale);
        cr.stroke()?;
    }
    cr.restore()
}

fn apply_selection_transform(cr: &Context, transform: SelectionTransform) {
    match transform {
        SelectionTransform::None => {}
        SelectionTransform::Move { dx, dy } => cr.translate(dx, dy),
        SelectionTransform::Scale { center, factor } => {
            cr.translate(center.x, center.y);
            cr.scale(factor, factor);
            cr.translate(-center.x, -center.y);
        }
        SelectionTransform::Rotate { center, radians } => {
            cr.translate(center.x, center.y);
            cr.rotate(radians);
            cr.translate(-center.x, -center.y);
        }
    }
}

fn build_selection_edge_path(cr: &Context, stroke: &Stroke) -> bool {
    match stroke.points.as_slice() {
        [] => false,
        [point] => {
            cr.new_sub_path();
            cr.arc(
                point.x,
                point.y,
                stroke.width_at(0).max(0.05) / 2.0 + 0.5,
                0.0,
                2.0 * PI,
            );
            true
        }
        _ => {
            if stroke.widths_pt.is_some() {
                let widths: Vec<f64> = (0..stroke.points.len())
                    .map(|index| stroke.width_at(index))
                    .collect();
                let (points, widths) = smooth_polyline(&stroke.points, &widths);
                build_outline_path(cr, &points, &widths, 0.5);
            } else {
                let widths = vec![stroke.base_width_pt.max(0.05); stroke.points.len()];
                build_outline_path(cr, &stroke.points, &widths, 0.5);
            }
            true
        }
    }
}

pub fn draw_marquee(
    cr: &Context,
    kind: MarqueeKind,
    start: Point,
    current: Point,
    points: &[Point],
    view: &ViewTransform,
) -> cairo::Result<()> {
    cr.save()?;
    apply_transform(cr, view)?;
    let logical_to_page = view.pt_to_logical().max(1e-6);
    let dash = 5.0 / logical_to_page;
    let gap = 4.0 / logical_to_page;
    cr.set_dash(&[dash, gap], 0.0);
    cr.set_line_width(1.5 / logical_to_page);
    match kind {
        MarqueeKind::Rect => {
            let (x0, y0) = (start.x.min(current.x), start.y.min(current.y));
            let (x1, y1) = (start.x.max(current.x), start.y.max(current.y));
            cr.rectangle(x0, y0, x1 - x0, y1 - y0);
        }
        MarqueeKind::Lasso => {
            if let Some(first) = points.first() {
                cr.move_to(first.x, first.y);
                for point in &points[1..] {
                    cr.line_to(point.x, point.y);
                }
                cr.line_to(current.x, current.y);
                cr.close_path();
            }
        }
    }
    cr.set_source_rgba(
        SELECTION_COLOR.0,
        SELECTION_COLOR.1,
        SELECTION_COLOR.2,
        0.10,
    );
    cr.fill_preserve()?;
    cr.set_source_rgba(
        SELECTION_COLOR.0,
        SELECTION_COLOR.1,
        SELECTION_COLOR.2,
        0.95,
    );
    cr.stroke()?;
    cr.set_dash(&[], 0.0);
    cr.restore()
}

pub fn draw_layer_thumbnail(
    cr: &Context,
    layer: &Layer,
    page_width_pt: f64,
    page_height_pt: f64,
    area_width: f64,
    area_height: f64,
) -> cairo::Result<()> {
    cr.save()?;
    cr.set_operator(cairo::Operator::Source);
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.paint()?;
    cr.set_operator(cairo::Operator::Over);
    let view = ViewTransform::for_area(area_width, area_height, page_width_pt, page_height_pt);
    draw_layer(cr, layer, &view)?;
    cr.restore()
}

fn apply_transform(cr: &Context, view: &ViewTransform) -> cairo::Result<()> {
    let (origin_x, origin_y) = view.origin_logical();
    cr.translate(origin_x, origin_y);
    cr.scale(view.pt_to_logical(), view.pt_to_logical());
    Ok(())
}

fn paint_stroke(cr: &Context, stroke: &Stroke) -> cairo::Result<()> {
    if stroke.points.is_empty() {
        return Ok(());
    }
    let color = stroke.color;
    cr.set_source_rgba(
        color.r as f64 / 255.0,
        color.g as f64 / 255.0,
        color.b as f64 / 255.0,
        color.a as f64 / 255.0,
    );
    paint_stroke_extra(cr, stroke, 0.0)
}

fn paint_stroke_extra(cr: &Context, stroke: &Stroke, extra: f64) -> cairo::Result<()> {
    match &stroke.widths_pt {
        None => paint_constant_stroke(cr, stroke, extra),
        Some(_) => paint_variable_stroke(cr, stroke, extra),
    }
}

fn paint_constant_stroke(cr: &Context, stroke: &Stroke, extra: f64) -> cairo::Result<()> {
    let width = stroke.base_width_pt.max(0.05);
    if stroke.points.len() == 1 {
        let point = stroke.points[0];
        cr.new_sub_path();
        cr.arc(point.x, point.y, width / 2.0 + extra, 0.0, 2.0 * PI);
        return cr.fill();
    }
    cr.new_path();
    cr.move_to(stroke.points[0].x, stroke.points[0].y);
    for point in &stroke.points[1..] {
        cr.line_to(point.x, point.y);
    }
    cr.set_line_width(width + 2.0 * extra);
    cr.stroke()
}

fn paint_variable_stroke(cr: &Context, stroke: &Stroke, extra: f64) -> cairo::Result<()> {
    if stroke.points.len() == 1 {
        let point = stroke.points[0];
        cr.new_sub_path();
        cr.arc(
            point.x,
            point.y,
            stroke.width_at(0).max(0.05) / 2.0 + extra,
            0.0,
            2.0 * PI,
        );
        return cr.fill();
    }

    // 采样点之间直接连直线，快速书写时点距大会出现明显折痕；
    // 先用向心 Catmull-Rom 加密成平滑折线，再生成变宽轮廓。
    let widths: Vec<f64> = (0..stroke.points.len())
        .map(|index| stroke.width_at(index))
        .collect();
    let (points, widths) = smooth_polyline(&stroke.points, &widths);
    build_outline_path(cr, &points, &widths, extra);
    cr.fill()
}

fn build_outline_path(cr: &Context, points: &[Point], widths: &[f64], extra: f64) {
    let count = points.len();
    let directions: Vec<(f64, f64)> = (0..count)
        .map(|index| direction_at(points, index))
        .collect();
    let mut left = Vec::with_capacity(count);
    let mut right = Vec::with_capacity(count);
    for index in 0..count {
        let half = widths[index].max(0.05) / 2.0 + extra;
        let (dx, dy) = directions[index];
        let normal = (-dy, dx);
        let point = points[index];
        left.push(Point::new(
            point.x + normal.0 * half,
            point.y + normal.1 * half,
        ));
        right.push(Point::new(
            point.x - normal.0 * half,
            point.y - normal.1 * half,
        ));
    }

    let last = count - 1;
    cr.new_path();
    cr.move_to(left[0].x, left[0].y);
    for point in &left[1..] {
        cr.line_to(point.x, point.y);
    }

    let end_half = widths[last].max(0.05) / 2.0 + extra;
    for point in cap_points(
        points[last],
        end_half,
        left[last],
        right[last],
        Point::new(directions[last].0, directions[last].1),
    ) {
        cr.line_to(point.x, point.y);
    }

    for point in right.iter().rev() {
        cr.line_to(point.x, point.y);
    }

    let start_half = widths[0].max(0.05) / 2.0 + extra;
    let backward = Point::new(-directions[0].0, -directions[0].1);
    for point in cap_points(points[0], start_half, right[0], left[0], backward) {
        cr.line_to(point.x, point.y);
    }

    cr.close_path();
}

/// 用向心 Catmull-Rom（alpha = 0.5）把采样点加密成平滑折线。
///
/// 参数 - 曲线经过每个原始采样点，不会像 B 样条那样把笔迹“削圆”；
/// 向心参数化又能避免均匀参数化在急转处的过冲和自交。
/// 线宽沿每段线性插值。
fn smooth_polyline(points: &[Point], widths: &[f64]) -> (Vec<Point>, Vec<f64>) {
    let count = points.len();
    if count < 3 {
        return (points.to_vec(), widths.to_vec());
    }

    let mut out_points = Vec::with_capacity(count * 2);
    let mut out_widths = Vec::with_capacity(count * 2);
    out_points.push(points[0]);
    out_widths.push(widths[0]);

    for index in 0..count - 1 {
        let p0 = points[index.saturating_sub(1)];
        let p1 = points[index];
        let p2 = points[index + 1];
        let p3 = points[(index + 2).min(count - 1)];
        let w1 = widths[index];
        let w2 = widths[index + 1];

        let length = (p2.x - p1.x).hypot(p2.y - p1.y);
        let steps = if length.is_finite() {
            ((length / SMOOTH_MAX_STEP_PT).ceil() as usize).clamp(1, 64)
        } else {
            1
        };
        for step in 1..steps {
            let t = step as f64 / steps as f64;
            let point = catmull_rom_point(p0, p1, p2, p3, t);
            if point.is_finite() {
                out_points.push(point);
                out_widths.push(w1 + (w2 - w1) * t);
            }
        }
        out_points.push(p2);
        out_widths.push(w2);

        if out_points.len() >= SMOOTH_MAX_POINTS {
            break;
        }
    }

    (out_points, out_widths)
}

/// 向心 Catmull-Rom 求值（Barry-Goldman 递归形式）。
fn catmull_rom_point(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let knot = |a: Point, b: Point| (b.x - a.x).hypot(b.y - a.y).sqrt().max(1e-6);
    let t0 = 0.0;
    let t1 = t0 + knot(p0, p1);
    let t2 = t1 + knot(p1, p2);
    let t3 = t2 + knot(p2, p3);
    let tt = t1 + (t2 - t1) * t.clamp(0.0, 1.0);

    let mix = |a: Point, b: Point, ta: f64, tb: f64, at: f64| {
        let d = (tb - ta).max(1e-9);
        let wa = (tb - at) / d;
        let wb = (at - ta) / d;
        Point::new(a.x * wa + b.x * wb, a.y * wa + b.y * wb)
    };

    let a1 = mix(p0, p1, t0, t1, tt);
    let a2 = mix(p1, p2, t1, t2, tt);
    let a3 = mix(p2, p3, t2, t3, tt);
    let b1 = mix(a1, a2, t0, t2, tt);
    let b2 = mix(a2, a3, t1, t3, tt);
    mix(b1, b2, t1, t2, tt)
}

fn direction_at(points: &[Point], index: usize) -> (f64, f64) {
    let count = points.len();
    let (from, to) = if count < 2 {
        (points[0], Point::new(points[0].x + 1.0, points[0].y))
    } else if index == 0 {
        (points[0], points[1])
    } else if index == count - 1 {
        (points[count - 2], points[count - 1])
    } else {
        (points[index - 1], points[index + 1])
    };
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let length = dx.hypot(dy);
    if length <= 1e-9 {
        if index > 0 {
            direction_at(points, index - 1)
        } else {
            (1.0, 0.0)
        }
    } else {
        (dx / length, dy / length)
    }
}

fn cap_points(center: Point, radius: f64, from: Point, to: Point, away: Point) -> Vec<Point> {
    let start = (from.y - center.y).atan2(from.x - center.x);
    let end = (to.y - center.y).atan2(to.x - center.x);
    let mut delta = normalize_angle(end - start);
    let mid = start + delta / 2.0;
    if mid.cos() * away.x + mid.sin() * away.y < 0.0 {
        delta -= delta.signum() * 2.0 * PI;
    }
    (1..CAP_SEGMENTS)
        .map(|step| {
            let angle = start + delta * step as f64 / CAP_SEGMENTS as f64;
            Point::new(
                center.x + angle.cos() * radius,
                center.y + angle.sin() * radius,
            )
        })
        .collect()
}

fn normalize_angle(angle: f64) -> f64 {
    let mut value = angle % (2.0 * PI);
    if value > PI {
        value -= 2.0 * PI;
    } else if value < -PI {
        value += 2.0 * PI;
    }
    value
}

#[cfg(test)]
mod smooth_tests {
    use super::*;

    fn sparse() -> (Vec<Point>, Vec<f64>) {
        (
            vec![
                Point::new(40.0, 60.0),
                Point::new(75.0, 95.0),
                Point::new(115.0, 45.0),
                Point::new(150.0, 90.0),
                Point::new(190.0, 50.0),
            ],
            vec![2.0, 3.0, 2.2, 3.2, 2.0],
        )
    }

    #[test]
    fn smoothing_subdivides_sparse_segments_and_keeps_endpoints() {
        let (points, widths) = sparse();
        let (smoothed, smoothed_widths) = smooth_polyline(&points, &widths);
        assert!(
            smoothed.len() > points.len() * 3,
            "稀疏采样应被明显加密，实际 {} -> {}",
            points.len(),
            smoothed.len()
        );
        assert_eq!(smoothed.first().copied(), Some(points[0]));
        assert_eq!(smoothed.last().copied(), Some(*points.last().unwrap()));
        assert_eq!(smoothed_widths.len(), smoothed.len());
        assert!(
            smoothed.iter().all(|point| point.is_finite()),
            "插值结果必须有限"
        );
    }

    #[test]
    fn smoothing_does_not_overshoot_wildly() {
        // 尖锐折角：插值点不能偏离相邻控制点连线太远（否则会出现鼓包/自交）
        let points = vec![
            Point::new(40.0, 160.0),
            Point::new(120.0, 160.0),
            Point::new(120.0, 220.0),
            Point::new(200.0, 160.0),
        ];
        let widths = vec![2.5; 4];
        let (smoothed, _) = smooth_polyline(&points, &widths);
        for point in &smoothed {
            // 所有插值点都应落在控制点包围盒外扩 12pt 的范围内
            assert!(
                point.x >= 28.0 && point.x <= 212.0 && point.y >= 148.0 && point.y <= 232.0,
                "插值点越界: {point:?}"
            );
        }
    }

    #[test]
    fn smooth_polyline_passes_through_original_points() {
        let (points, widths) = sparse();
        let (smoothed, _) = smooth_polyline(&points, &widths);
        for original in &points {
            let nearest = smoothed
                .iter()
                .map(|point| (point.x - original.x).hypot(point.y - original.y))
                .fold(f64::INFINITY, f64::min);
            assert!(nearest < 1e-6, "曲线必须经过原始采样点 {original:?}");
        }
    }

    #[test]
    fn smoothing_handles_degenerate_input() {
        // 少于 3 个点：原样返回
        let few = vec![Point::new(0.0, 0.0), Point::new(10.0, 10.0)];
        let (smoothed, widths) = smooth_polyline(&few, &[1.0, 1.0]);
        assert_eq!(smoothed.len(), 2);
        assert_eq!(widths.len(), 2);

        // 重复点（零长度段）不应该产生 NaN
        let repeated = vec![
            Point::new(5.0, 5.0),
            Point::new(5.0, 5.0),
            Point::new(5.0, 5.0),
            Point::new(20.0, 5.0),
        ];
        let (smoothed, _) = smooth_polyline(&repeated, &[2.0; 4]);
        assert!(smoothed.iter().all(|point| point.is_finite()));
    }

    #[test]
    fn catmull_rom_hits_endpoints() {
        let p0 = Point::new(0.0, 0.0);
        let p1 = Point::new(10.0, 0.0);
        let p2 = Point::new(20.0, 10.0);
        let p3 = Point::new(30.0, 0.0);
        let start = catmull_rom_point(p0, p1, p2, p3, 0.0);
        let end = catmull_rom_point(p0, p1, p2, p3, 1.0);
        assert!((start.x - p1.x).abs() < 1e-6 && (start.y - p1.y).abs() < 1e-6);
        assert!((end.x - p2.x).abs() < 1e-6 && (end.y - p2.y).abs() < 1e-6);
    }
}
