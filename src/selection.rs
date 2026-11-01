use crate::model::{point_segment_distance, stroke_hits_segment, Document, Point, Rgba, Stroke};

pub type Rect = (f64, f64, f64, f64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarqueeKind {
    Rect,
    Lasso,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SelectionTransform {
    None,
    Move { dx: f64, dy: f64 },
    Scale { center: Point, factor: f64 },
    Rotate { center: Point, radians: f64 },
}

impl SelectionTransform {
    pub fn point(self, point: Point) -> Point {
        match self {
            SelectionTransform::None => point,
            SelectionTransform::Move { dx, dy } => Point::new(point.x + dx, point.y + dy),
            SelectionTransform::Scale { center, factor } => Point::new(
                center.x + (point.x - center.x) * factor,
                center.y + (point.y - center.y) * factor,
            ),
            SelectionTransform::Rotate { center, radians } => {
                let (sin, cos) = radians.sin_cos();
                let dx = point.x - center.x;
                let dy = point.y - center.y;
                Point::new(
                    center.x + dx * cos - dy * sin,
                    center.y + dx * sin + dy * cos,
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectedStroke {
    pub layer_id: u64,
    pub index: usize,
    pub stroke: Stroke,
}

#[derive(Debug, Clone, Default)]
pub struct Selection {
    items: Vec<SelectedStroke>,
}

impl Selection {
    pub fn capture(document: &Document, candidates: &[(u64, usize)]) -> Self {
        let items = candidates
            .iter()
            .filter_map(|(layer_id, index)| {
                let layer = document.layers.iter().find(|layer| layer.id == *layer_id)?;
                let stroke = layer.strokes.get(*index)?;
                Some(SelectedStroke {
                    layer_id: *layer_id,
                    index: *index,
                    stroke: stroke.clone(),
                })
            })
            .collect();
        Self { items }
    }

    pub fn items(&self) -> &[SelectedStroke] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn bounds_pt(&self) -> Option<Rect> {
        self.items
            .iter()
            .map(|item| item.stroke.bounds_pt())
            .reduce(union)
    }

    pub fn hit_test(&self, point: Point, tolerance_pt: f64) -> bool {
        self.items
            .iter()
            .any(|item| stroke_hits_segment(&item.stroke, point, point, tolerance_pt))
    }

    pub fn translate(&mut self, dx: f64, dy: f64) {
        for item in &mut self.items {
            for point in &mut item.stroke.points {
                point.x += dx;
                point.y += dy;
            }
        }
    }

    pub fn scale(&mut self, center: Point, factor: f64) {
        for item in &mut self.items {
            item.stroke = scaled_stroke(&item.stroke, center, factor);
        }
    }

    pub fn rotate(&mut self, center: Point, radians: f64) {
        for item in &mut self.items {
            item.stroke = rotated_stroke(&item.stroke, center, radians);
        }
    }

    pub fn recolor(&mut self, color: Rgba) {
        for item in &mut self.items {
            item.stroke.color = color;
        }
    }

    pub fn items_mut(&mut self) -> &mut [SelectedStroke] {
        &mut self.items
    }
}

pub fn scaled_stroke(stroke: &Stroke, center: Point, factor: f64) -> Stroke {
    let mut stroke = stroke.clone();
    for point in &mut stroke.points {
        point.x = center.x + (point.x - center.x) * factor;
        point.y = center.y + (point.y - center.y) * factor;
    }
    stroke.base_width_pt = (stroke.base_width_pt * factor).max(0.05);
    if let Some(widths) = &mut stroke.widths_pt {
        for width in widths {
            *width = (*width * factor).max(0.05);
        }
    }
    stroke
}

pub fn rotated_stroke(stroke: &Stroke, center: Point, radians: f64) -> Stroke {
    let mut stroke = stroke.clone();
    let (sin, cos) = radians.sin_cos();
    for point in &mut stroke.points {
        let dx = point.x - center.x;
        let dy = point.y - center.y;
        point.x = center.x + dx * cos - dy * sin;
        point.y = center.y + dx * sin + dy * cos;
    }
    stroke
}

pub fn rect_candidates(document: &Document, rect: Rect) -> Vec<(u64, usize)> {
    let rect = normalize_rect(rect);
    let mut candidates = Vec::new();
    for layer in document.layers.iter().filter(|layer| layer.visible) {
        for (index, stroke) in layer.strokes.iter().enumerate() {
            if rect_contains_rect(rect, stroke.bounds_pt()) {
                candidates.push((layer.id, index));
            }
        }
    }
    candidates
}

pub fn lasso_candidates(document: &Document, polygon: &[Point]) -> Vec<(u64, usize)> {
    if polygon.len() < 3 {
        return Vec::new();
    }
    let Some(polygon_bounds) = points_bounds(polygon) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for layer in document.layers.iter().filter(|layer| layer.visible) {
        for (index, stroke) in layer.strokes.iter().enumerate() {
            if !rects_overlap(polygon_bounds, stroke.bounds_pt()) {
                continue;
            }
            if stroke_inside_lasso(stroke, polygon) {
                candidates.push((layer.id, index));
            }
        }
    }
    candidates
}

fn stroke_inside_lasso(stroke: &Stroke, polygon: &[Point]) -> bool {
    let Some(points) = stroke.points.first().map(|_| stroke.points.as_slice()) else {
        return false;
    };
    let radius = stroke.max_width() / 2.0 + 0.5;
    for (index, point) in points.iter().enumerate() {
        if !disc_inside_polygon(*point, radius, polygon) {
            return false;
        }
        if let Some(next) = points.get(index + 1) {
            let length = point.distance(*next);
            let steps = (length / 2.0).ceil().max(1.0) as usize;
            for step in 1..steps {
                let t = step as f64 / steps as f64;
                let middle = Point::new(
                    point.x + (next.x - point.x) * t,
                    point.y + (next.y - point.y) * t,
                );
                if !disc_inside_polygon(middle, radius, polygon) {
                    return false;
                }
            }
        }
    }
    true
}

fn disc_inside_polygon(center: Point, radius: f64, polygon: &[Point]) -> bool {
    if !point_in_polygon(center, polygon) {
        return false;
    }
    for index in 0..8 {
        let angle = index as f64 * std::f64::consts::TAU / 8.0;
        let point = Point::new(
            center.x + angle.cos() * radius,
            center.y + angle.sin() * radius,
        );
        if !point_in_polygon(point, polygon) {
            return false;
        }
    }
    true
}

pub fn point_in_polygon(point: Point, polygon: &[Point]) -> bool {
    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];
    for &current in polygon {
        let crosses = (current.y > point.y) != (previous.y > point.y);
        if crosses {
            let x = (previous.x - current.x) * (point.y - current.y) / (previous.y - current.y)
                + current.x;
            if point.x < x {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

pub fn points_bounds(points: &[Point]) -> Option<Rect> {
    let first = points.first()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
    for point in &points[1..] {
        x0 = x0.min(point.x);
        y0 = y0.min(point.y);
        x1 = x1.max(point.x);
        y1 = y1.max(point.y);
    }
    Some((x0, y0, x1, y1))
}

pub fn normalize_rect(rect: Rect) -> Rect {
    (
        rect.0.min(rect.2),
        rect.1.min(rect.3),
        rect.0.max(rect.2),
        rect.1.max(rect.3),
    )
}

pub fn rect_contains_rect(outer: Rect, inner: Rect) -> bool {
    let outer = normalize_rect(outer);
    let inner = normalize_rect(inner);
    inner.0 >= outer.0 && inner.1 >= outer.1 && inner.2 <= outer.2 && inner.3 <= outer.3
}

pub fn rects_overlap(a: Rect, b: Rect) -> bool {
    let a = normalize_rect(a);
    let b = normalize_rect(b);
    a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
}

pub fn union(a: Rect, b: Rect) -> Rect {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

pub fn stroke_hit_point(stroke: &Stroke, point: Point, tolerance_pt: f64) -> bool {
    stroke_hits_segment(stroke, point, point, tolerance_pt)
}

pub fn stroke_distance(stroke: &Stroke, point: Point) -> f64 {
    match stroke.points.as_slice() {
        [] => f64::INFINITY,
        [single] => single.distance(point),
        points => points
            .windows(2)
            .map(|pair| point_segment_distance(point, pair[0], pair[1]))
            .fold(f64::INFINITY, f64::min),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(x0: f64, y0: f64, x1: f64, y1: f64) -> Stroke {
        Stroke::constant(
            Rgba::BLACK,
            2.0,
            vec![Point::new(x0, y0), Point::new(x1, y1)],
        )
    }

    fn document_with(strokes: Vec<Stroke>) -> Document {
        let mut document = Document::new(800.0, 600.0);
        document.layers[0].strokes = strokes;
        document
    }

    #[test]
    fn rect_requires_full_containment() {
        let document = document_with(vec![
            line(100.0, 100.0, 200.0, 200.0),
            line(150.0, 150.0, 260.0, 180.0),
        ]);
        let candidates = rect_candidates(&document, (50.0, 50.0, 250.0, 250.0));
        assert_eq!(candidates, vec![(document.layers[0].id, 0)]);
    }

    #[test]
    fn hidden_layers_are_not_selectable() {
        let mut document = document_with(vec![line(100.0, 100.0, 200.0, 200.0)]);
        document.layers[0].visible = false;
        assert!(rect_candidates(&document, (0.0, 0.0, 800.0, 600.0)).is_empty());
    }

    #[test]
    fn lasso_handles_concave_polygon() {
        let document = document_with(vec![
            line(10.0, 10.0, 20.0, 30.0),
            line(120.0, 30.0, 140.0, 50.0),
        ]);
        // 凹字形：左侧凹口内的笔迹不应被选中。
        let polygon = vec![
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 100.0),
            Point::new(70.0, 100.0),
            Point::new(70.0, 40.0),
            Point::new(30.0, 40.0),
            Point::new(30.0, 100.0),
            Point::new(0.0, 100.0),
        ];
        let candidates = lasso_candidates(&document, &polygon);
        assert_eq!(candidates, vec![(document.layers[0].id, 0)]);
    }

    #[test]
    fn selection_transforms_points_and_widths() {
        let document = document_with(vec![Stroke::variable(
            Rgba::BLACK,
            4.0,
            vec![2.0, 4.0],
            vec![Point::new(10.0, 10.0), Point::new(20.0, 20.0)],
        )]);
        let mut selection = Selection::capture(&document, &[(document.layers[0].id, 0)]);
        selection.translate(5.0, -5.0);
        selection.scale(Point::new(17.5, 12.5), 2.0);
        let item = &selection.items()[0];
        assert_eq!(item.stroke.points[0], Point::new(12.5, -2.5));
        assert_eq!(item.stroke.points[1], Point::new(32.5, 17.5));
        assert_eq!(item.stroke.base_width_pt, 8.0);
        assert_eq!(item.stroke.widths_pt, Some(vec![4.0, 8.0]));
    }

    #[test]
    fn hit_test_uses_stroke_width_and_tolerance() {
        let document = document_with(vec![line(0.0, 0.0, 100.0, 0.0)]);
        let selection = Selection::capture(&document, &[(document.layers[0].id, 0)]);
        assert!(selection.hit_test(Point::new(50.0, 3.0), 2.0));
        assert!(!selection.hit_test(Point::new(50.0, 20.0), 2.0));
    }
}
