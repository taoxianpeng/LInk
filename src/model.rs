use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const BLACK: Self = Self::new(0, 0, 0, 255);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        let hex = text.trim().trim_start_matches('#');
        let parse = |range: std::ops::Range<usize>| {
            hex.get(range)
                .and_then(|part| u8::from_str_radix(part, 16).ok())
        };
        match hex.len() {
            6 => Some(Self::new(parse(0..2)?, parse(2..4)?, parse(4..6)?, 255)),
            8 => Some(Self::new(
                parse(0..2)?,
                parse(2..4)?,
                parse(4..6)?,
                parse(6..8)?,
            )),
            _ => None,
        }
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
    }

    pub fn with_alpha(self, alpha: u8) -> Self {
        Self { a: alpha, ..self }
    }

    pub fn to_css(self) -> String {
        format!(
            "rgba({}, {}, {}, {})",
            self.r,
            self.g,
            self.b,
            self.a as f64 / 255.0
        )
    }
}

impl fmt::Display for Rgba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn distance(self, other: Self) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub color: Rgba,
    pub base_width_pt: f64,
    pub widths_pt: Option<Vec<f64>>,
    pub points: Vec<Point>,
}

impl Stroke {
    pub fn constant(color: Rgba, width_pt: f64, points: Vec<Point>) -> Self {
        Self {
            color,
            base_width_pt: width_pt,
            widths_pt: None,
            points,
        }
    }

    pub fn variable(
        color: Rgba,
        base_width_pt: f64,
        widths_pt: Vec<f64>,
        points: Vec<Point>,
    ) -> Self {
        debug_assert_eq!(widths_pt.len(), points.len());
        Self {
            color,
            base_width_pt,
            widths_pt: Some(widths_pt),
            points,
        }
    }

    pub fn width_at(&self, index: usize) -> f64 {
        match &self.widths_pt {
            Some(widths) => widths.get(index).copied().unwrap_or(self.base_width_pt),
            None => self.base_width_pt,
        }
    }

    /// 笔迹的页面坐标包围盒 `(x0, y0, x1, y1)`，已把线宽算进去。
    ///
    /// 用于"只重画某一小片区域"的裁剪判断：包围盒不相交的笔迹可以直接跳过。
    pub fn bounds_pt(&self) -> (f64, f64, f64, f64) {
        let margin = self.max_width() / 2.0 + 1.0;
        let Some(first) = self.points.first() else {
            return (0.0, 0.0, 0.0, 0.0);
        };
        let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
        for point in &self.points[1..] {
            x0 = x0.min(point.x);
            y0 = y0.min(point.y);
            x1 = x1.max(point.x);
            y1 = y1.max(point.y);
        }
        (x0 - margin, y0 - margin, x1 + margin, y1 + margin)
    }

    pub fn max_width(&self) -> f64 {
        self.widths_pt
            .as_ref()
            .map(|widths| widths.iter().copied().fold(0.0, f64::max))
            .unwrap_or(self.base_width_pt)
    }
}

#[derive(Debug, Clone)]
pub struct Layer {
    /// 稳定的图层标识。撤回记录用它定位图层，这样增删/移动图层后索引错位也不会撤错层。
    /// 不参与相等比较，也不写入 xopp（每次加载重新分配）。
    pub id: u64,
    pub name: String,
    pub strokes: Vec<Stroke>,
    /// 是否显示。仅影响屏幕渲染与导出，不写入 xopp（Xournal++ 没有图层可见性）。
    pub visible: bool,
}

impl PartialEq for Layer {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.strokes == other.strokes && self.visible == other.visible
    }
}

impl Layer {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: next_layer_id(),
            name: name.into(),
            strokes: Vec::new(),
            visible: true,
        }
    }
}

fn next_layer_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub width_pt: f64,
    pub height_pt: f64,
    pub layers: Vec<Layer>,
    pub active: usize,
}

impl Document {
    pub fn new(width_pt: f64, height_pt: f64) -> Self {
        let width_pt = if width_pt.is_finite() && width_pt > 0.0 {
            width_pt
        } else {
            800.0
        };
        let height_pt = if height_pt.is_finite() && height_pt > 0.0 {
            height_pt
        } else {
            600.0
        };
        Self {
            width_pt,
            height_pt,
            layers: vec![Layer::new("图层 1")],
            active: 0,
        }
    }

    /// 按稳定 id 找图层，返回其索引。
    pub fn layer_index_by_id(&self, id: u64) -> Option<usize> {
        self.layers.iter().position(|layer| layer.id == id)
    }

    pub fn active_layer(&self) -> &Layer {
        &self.layers[self.active]
    }

    pub fn active_layer_mut(&mut self) -> &mut Layer {
        &mut self.layers[self.active]
    }

    pub fn add_layer_above_active(&mut self) {
        let index = (self.active + 1).min(self.layers.len());
        let name = format!("图层 {}", self.layers.len() + 1);
        self.layers.insert(index, Layer::new(name));
        self.active = index;
    }

    pub fn remove_active_layer(&mut self) -> bool {
        if self.layers.len() <= 1 {
            return false;
        }
        self.layers.remove(self.active);
        self.active = self.active.min(self.layers.len() - 1);
        true
    }

    pub fn can_move_active(&self, up: bool) -> bool {
        if up {
            self.active + 1 < self.layers.len()
        } else {
            self.active > 0
        }
    }

    pub fn move_active_layer(&mut self, up: bool) -> bool {
        if !self.can_move_active(up) {
            return false;
        }
        let target = if up { self.active + 1 } else { self.active - 1 };
        self.layers.swap(self.active, target);
        self.active = target;
        true
    }

    /// 切换指定图层的显示状态，返回是否真的发生了变化。
    pub fn set_layer_visible(&mut self, index: usize, visible: bool) -> bool {
        let Some(layer) = self.layers.get_mut(index) else {
            return false;
        };
        if layer.visible == visible {
            return false;
        }
        layer.visible = visible;
        true
    }

    pub fn toggle_layer_visible(&mut self, index: usize) -> bool {
        match self.layers.get(index) {
            Some(layer) => self.set_layer_visible(index, !layer.visible),
            None => false,
        }
    }

    /// 重命名图层。空名字或未变化时返回 `false`。
    pub fn rename_layer(&mut self, index: usize, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let Some(layer) = self.layers.get_mut(index) else {
            return false;
        };
        if layer.name == name {
            return false;
        }
        layer.name = name.to_string();
        true
    }

    pub fn erase_segment_active(&mut self, from: Point, to: Point, tolerance_pt: f64) -> usize {
        self.erase_segment_active_recorded(from, to, tolerance_pt)
            .len()
    }

    /// 同 [`Self::erase_segment_active`]，但把被删掉的笔迹连同原始位置一起返回，
    /// 供撤回栈恢复。
    pub fn erase_segment_active_recorded(
        &mut self,
        from: Point,
        to: Point,
        tolerance_pt: f64,
    ) -> Vec<(usize, Stroke)> {
        let layer = &mut self.layers[self.active];
        let mut removed = Vec::new();
        let mut kept = Vec::with_capacity(layer.strokes.len());
        for (index, stroke) in std::mem::take(&mut layer.strokes).into_iter().enumerate() {
            if stroke_hits_segment(&stroke, from, to, tolerance_pt) {
                removed.push((index, stroke));
            } else {
                kept.push(stroke);
            }
        }
        layer.strokes = kept;
        removed
    }
}

pub fn stroke_hits_segment(stroke: &Stroke, from: Point, to: Point, tolerance_pt: f64) -> bool {
    let radius = stroke.max_width() / 2.0 + tolerance_pt.max(0.0);
    match stroke.points.as_slice() {
        [] => false,
        [point] => point_segment_distance(*point, from, to) <= radius,
        points => points
            .windows(2)
            .any(|pair| segments_near(pair[0], pair[1], from, to, radius)),
    }
}

fn segments_near(a1: Point, a2: Point, b1: Point, b2: Point, radius: f64) -> bool {
    segments_intersect(a1, a2, b1, b2)
        || point_segment_distance(a1, b1, b2) <= radius
        || point_segment_distance(a2, b1, b2) <= radius
        || point_segment_distance(b1, a1, a2) <= radius
        || point_segment_distance(b2, a1, a2) <= radius
}

fn segments_intersect(a: Point, b: Point, c: Point, d: Point) -> bool {
    let cross =
        |p: Point, q: Point, r: Point| (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x);
    let d1 = cross(c, d, a);
    let d2 = cross(c, d, b);
    let d3 = cross(a, b, c);
    let d4 = cross(a, b, d);
    ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
}

pub fn point_segment_distance(point: Point, start: Point, end: Point) -> f64 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_sq = dx * dx + dy * dy;
    if length_sq <= f64::EPSILON {
        return point.distance(start);
    }
    let t = (((point.x - start.x) * dx + (point.y - start.y) * dy) / length_sq).clamp(0.0, 1.0);
    point.distance(Point::new(start.x + t * dx, start.y + t * dy))
}
