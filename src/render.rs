use std::f64::consts::PI;

use crate::model::{Document, Layer, Point, Stroke};
use crate::paint::{Canvas, Cap, Color, Join, Path2, PathBuilder2, RectF};
use crate::selection::{MarqueeKind, Rect, SelectionTransform};
use crate::view::ViewTransform;

const SELECTION_COLOR: (f64, f64, f64, f64) = (0.31765, 0.27843, 0.89804, 1.0);
const SELECTION_HALO: (f64, f64, f64, f64) = (1.0, 1.0, 1.0, 0.92);

const CAP_SEGMENTS: usize = 10;
/// 平滑插值的最大步长（页面 pt）。约等于 2 个逻辑像素，足够消除折痕。
const SMOOTH_MAX_STEP_PT: f64 = 1.5;
/// 平滑后的点数上限，避免极长笔迹把每帧成本拉爆。
const SMOOTH_MAX_POINTS: usize = 24_000;

pub fn draw_document(canvas: &mut dyn Canvas, document: &Document, view: &ViewTransform) {
    canvas.save();
    apply_transform(canvas, view);
    canvas.set_line_cap(Cap::Round);
    canvas.set_line_join(Join::Round);
    for layer in &document.layers {
        if !layer.visible {
            continue;
        }
        for stroke in &layer.strokes {
            paint_stroke(canvas, stroke);
        }
    }
    canvas.restore();
}

/// 只画 `layer_index` **之上**的那些图层的笔迹。
///
/// 草稿（正在画的那一笔）属于当前图层，必须夹在"它下面的层"和"它上面的层"之间；
/// 否则在非顶层书写时，笔迹会浮在所有图层之上，等到提交（重新合成缓存）才跳回正确位置。
///
/// `cull_pt` 是本帧要重绘的页面区域，包围盒不相交的笔迹直接跳过 —— 这条路径每帧都会走，
/// 上层笔迹多时靠它把成本压在一小块区域内。
pub fn draw_layers_above(
    canvas: &mut dyn Canvas,
    document: &Document,
    layer_index: usize,
    view: &ViewTransform,
    cull_pt: Option<(f64, f64, f64, f64)>,
) {
    let above = document.layers.len().saturating_sub(layer_index + 1);
    if above == 0 {
        return;
    }
    canvas.save();
    apply_transform(canvas, view);
    canvas.set_line_cap(Cap::Round);
    canvas.set_line_join(Join::Round);
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
            paint_stroke(canvas, stroke);
        }
    }
    canvas.restore();
}

fn rects_overlap(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
    a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
}

pub fn draw_stroke(canvas: &mut dyn Canvas, stroke: &Stroke, view: &ViewTransform) {
    canvas.save();
    apply_transform(canvas, view);
    canvas.set_line_cap(Cap::Round);
    canvas.set_line_join(Join::Round);
    paint_stroke(canvas, stroke);
    canvas.restore();
}

pub fn draw_layer(canvas: &mut dyn Canvas, layer: &Layer, view: &ViewTransform) {
    canvas.save();
    apply_transform(canvas, view);
    canvas.set_line_cap(Cap::Round);
    canvas.set_line_join(Join::Round);
    for stroke in &layer.strokes {
        paint_stroke(canvas, stroke);
    }
    canvas.restore();
}

pub fn draw_selection(
    canvas: &mut dyn Canvas,
    strokes: &[&Stroke],
    view: &ViewTransform,
    transform: SelectionTransform,
) {
    canvas.save();
    apply_transform(canvas, view);
    apply_selection_transform(canvas, transform);
    let scale = view.pt_to_logical().max(1e-6);
    canvas.set_dash(&[5.0 / scale, 4.0 / scale], 0.0);
    canvas.set_line_cap(Cap::Round);
    canvas.set_line_join(Join::Round);
    for stroke in strokes {
        let Some(path) = build_selection_edge_path(stroke) else {
            continue;
        };
        canvas.set_color(rgba(
            SELECTION_HALO.0,
            SELECTION_HALO.1,
            SELECTION_HALO.2,
            0.85,
        ));
        canvas.set_line_width(2.4 / scale);
        canvas.stroke_path(&path);
        canvas.set_color(rgba(
            SELECTION_COLOR.0,
            SELECTION_COLOR.1,
            SELECTION_COLOR.2,
            0.95,
        ));
        canvas.set_line_width(1.3 / scale);
        canvas.stroke_path(&path);
    }
    canvas.set_dash(&[], 0.0);
    canvas.restore();
}

pub fn draw_selection_box(
    canvas: &mut dyn Canvas,
    bounds: Rect,
    transform: SelectionTransform,
    view: &ViewTransform,
) {
    canvas.save();
    apply_transform(canvas, view);
    apply_selection_transform(canvas, transform);
    let scale = view.pt_to_logical().max(1e-6);
    let corners = [
        Point::new(bounds.0, bounds.1),
        Point::new(bounds.2, bounds.1),
        Point::new(bounds.2, bounds.3),
        Point::new(bounds.0, bounds.3),
    ];
    canvas.set_color(rgba(
        SELECTION_COLOR.0,
        SELECTION_COLOR.1,
        SELECTION_COLOR.2,
        0.95,
    ));
    canvas.set_line_width(1.0 / scale);
    let mut outline = PathBuilder2::new();
    outline.move_to(corners[0].x, corners[0].y);
    for point in &corners[1..] {
        outline.line_to(point.x, point.y);
    }
    outline.close();
    if let Some(outline) = outline.finish() {
        canvas.stroke_path(&outline);
    }

    let half = 3.0 / scale;
    let mut handles = Vec::with_capacity(8);
    handles.extend_from_slice(&corners);
    for index in 0..4 {
        let a = corners[index];
        let b = corners[(index + 1) % 4];
        handles.push(Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0));
    }
    for point in handles {
        let mut handle = PathBuilder2::new();
        handle.rect(point.x - half, point.y - half, half * 2.0, half * 2.0);
        let Some(handle) = handle.finish() else {
            continue;
        };
        canvas.set_color(Color::WHITE);
        canvas.fill_path(&handle);
        canvas.set_color(rgba(
            SELECTION_COLOR.0,
            SELECTION_COLOR.1,
            SELECTION_COLOR.2,
            1.0,
        ));
        canvas.set_line_width(1.0 / scale);
        canvas.stroke_path(&handle);
    }
    canvas.restore();
}

fn apply_selection_transform(canvas: &mut dyn Canvas, transform: SelectionTransform) {
    match transform {
        SelectionTransform::None => {}
        SelectionTransform::Move { dx, dy } => canvas.translate(dx, dy),
        SelectionTransform::Scale { center, factor } => {
            canvas.translate(center.x, center.y);
            canvas.scale(factor, factor);
            canvas.translate(-center.x, -center.y);
        }
        SelectionTransform::Rotate { center, radians } => {
            canvas.translate(center.x, center.y);
            canvas.rotate(radians);
            canvas.translate(-center.x, -center.y);
        }
    }
}

fn build_selection_edge_path(stroke: &Stroke) -> Option<Path2> {
    match stroke.points.as_slice() {
        [] => None,
        [point] => {
            let mut builder = PathBuilder2::new();
            builder.circle(
                point.x,
                point.y,
                stroke.width_at(0).max(0.05) / 2.0 + 0.5,
            );
            builder.finish()
        }
        _ => {
            let mut builder = PathBuilder2::new();
            if stroke.widths_pt.is_some() {
                let widths: Vec<f64> = (0..stroke.points.len())
                    .map(|index| stroke.width_at(index))
                    .collect();
                let (points, widths) = smooth_polyline(&stroke.points, &widths);
                build_outline_path(&mut builder, &points, &widths, 0.5);
            } else {
                let widths = vec![stroke.base_width_pt.max(0.05); stroke.points.len()];
                build_outline_path(&mut builder, &stroke.points, &widths, 0.5);
            }
            builder.finish()
        }
    }
}

pub fn draw_marquee(
    canvas: &mut dyn Canvas,
    kind: MarqueeKind,
    start: Point,
    current: Point,
    points: &[Point],
    view: &ViewTransform,
) {
    canvas.save();
    apply_transform(canvas, view);
    let logical_to_page = view.pt_to_logical().max(1e-6);
    let dash = 5.0 / logical_to_page;
    let gap = 4.0 / logical_to_page;
    canvas.set_dash(&[dash, gap], 0.0);
    canvas.set_line_width(1.5 / logical_to_page);

    let mut builder = PathBuilder2::new();
    match kind {
        MarqueeKind::Rect => {
            let (x0, y0) = (start.x.min(current.x), start.y.min(current.y));
            let (x1, y1) = (start.x.max(current.x), start.y.max(current.y));
            builder.rect(x0, y0, x1 - x0, y1 - y0);
        }
        MarqueeKind::Lasso => {
            if let Some(first) = points.first() {
                builder.move_to(first.x, first.y);
                for point in &points[1..] {
                    builder.line_to(point.x, point.y);
                }
                builder.line_to(current.x, current.y);
                builder.close();
            }
        }
    }
    if let Some(path) = builder.finish() {
        canvas.set_color(rgba(
            SELECTION_COLOR.0,
            SELECTION_COLOR.1,
            SELECTION_COLOR.2,
            0.10,
        ));
        canvas.fill_path(&path);
        canvas.set_color(rgba(
            SELECTION_COLOR.0,
            SELECTION_COLOR.1,
            SELECTION_COLOR.2,
            0.95,
        ));
        canvas.stroke_path(&path);
    }
    canvas.set_dash(&[], 0.0);
    canvas.restore();
}

pub fn draw_layer_thumbnail(
    canvas: &mut dyn Canvas,
    layer: &Layer,
    page_width_pt: f64,
    page_height_pt: f64,
    area_width: f64,
    area_height: f64,
) {
    canvas.save();
    canvas.paint_with(Color::WHITE, crate::paint::Blend::Source);
    let view = ViewTransform::for_area(area_width, area_height, page_width_pt, page_height_pt);
    draw_layer(canvas, layer, &view);
    canvas.restore();
}

fn apply_transform(canvas: &mut dyn Canvas, view: &ViewTransform) {
    let (origin_x, origin_y) = view.origin_logical();
    canvas.translate(origin_x, origin_y);
    canvas.scale(view.pt_to_logical(), view.pt_to_logical());
}

fn paint_stroke(canvas: &mut dyn Canvas, stroke: &Stroke) {
    if stroke.points.is_empty() {
        return;
    }
    let color = stroke.color;
    canvas.set_color(Color::new(color.r, color.g, color.b, color.a));
    paint_stroke_extra(canvas, stroke, 0.0);
}

fn paint_stroke_extra(canvas: &mut dyn Canvas, stroke: &Stroke, extra: f64) {
    match &stroke.widths_pt {
        None => paint_constant_stroke(canvas, stroke, extra),
        Some(_) => paint_variable_stroke(canvas, stroke, extra),
    }
}

fn paint_constant_stroke(canvas: &mut dyn Canvas, stroke: &Stroke, extra: f64) {
    let width = stroke.base_width_pt.max(0.05);
    if stroke.points.len() == 1 {
        let point = stroke.points[0];
        let mut builder = PathBuilder2::new();
        builder.circle(point.x, point.y, width / 2.0 + extra);
        if let Some(path) = builder.finish() {
            canvas.fill_path(&path);
        }
        return;
    }
    let mut builder = PathBuilder2::new();
    builder.move_to(stroke.points[0].x, stroke.points[0].y);
    for point in &stroke.points[1..] {
        builder.line_to(point.x, point.y);
    }
    let Some(path) = builder.finish() else {
        return;
    };
    canvas.set_line_width(width + 2.0 * extra);
    canvas.stroke_path(&path);
}

fn paint_variable_stroke(canvas: &mut dyn Canvas, stroke: &Stroke, extra: f64) {
    if stroke.points.len() == 1 {
        let point = stroke.points[0];
        let mut builder = PathBuilder2::new();
        builder.circle(
            point.x,
            point.y,
            stroke.width_at(0).max(0.05) / 2.0 + extra,
        );
        if let Some(path) = builder.finish() {
            canvas.fill_path(&path);
        }
        return;
    }

    // 采样点之间直接连直线，快速书写时点距大会出现明显折痕；
    // 先用向心 Catmull-Rom 加密成平滑折线，再生成变宽轮廓。
    let widths: Vec<f64> = (0..stroke.points.len())
        .map(|index| stroke.width_at(index))
        .collect();
    let (points, widths) = smooth_polyline(&stroke.points, &widths);
    let mut builder = PathBuilder2::new();
    build_outline_path(&mut builder, &points, &widths, extra);
    if let Some(path) = builder.finish() {
        canvas.fill_path(&path);
    }
}

fn build_outline_path(
    builder: &mut PathBuilder2,
    points: &[Point],
    widths: &[f64],
    extra: f64,
) {
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
    builder.move_to(left[0].x, left[0].y);
    for point in &left[1..] {
        builder.line_to(point.x, point.y);
    }

    let end_half = widths[last].max(0.05) / 2.0 + extra;
    for point in cap_points(
        points[last],
        end_half,
        left[last],
        right[last],
        Point::new(directions[last].0, directions[last].1),
    ) {
        builder.line_to(point.x, point.y);
    }

    for point in right.iter().rev() {
        builder.line_to(point.x, point.y);
    }

    let start_half = widths[0].max(0.05) / 2.0 + extra;
    let backward = Point::new(-directions[0].0, -directions[0].1);
    for point in cap_points(points[0], start_half, right[0], left[0], backward) {
        builder.line_to(point.x, point.y);
    }

    builder.close();
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

fn rgba(r: f64, g: f64, b: f64, a: f64) -> Color {
    Color::from_rgba_f64(r, g, b, a)
}

/// 供缓存层判断某块区域是否有笔迹需要重绘（缩略图/裁剪共用）。
pub fn stroke_bounds_logical(stroke: &Stroke, view: &ViewTransform) -> Option<RectF> {
    if stroke.points.is_empty() {
        return None;
    }
    let bounds = stroke.bounds_pt();
    let (x0, y0) = view.page_to_logical(Point::new(bounds.0, bounds.1));
    let (x1, y1) = view.page_to_logical(Point::new(bounds.2, bounds.3));
    Some(RectF::from_bounds(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)))
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
