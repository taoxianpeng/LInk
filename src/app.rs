use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use crate::input::{pressure_width, InputEvent, PointerSample, ToolKind};
use crate::model::{Document, Point, Rgba, Stroke};
use crate::view::ViewTransform;

const MIN_SAMPLE_DISTANCE_LOGICAL: f64 = 0.5;
const ERASE_TOLERANCE_PT: f64 = 2.0;
const MIN_STROKE_WIDTH_PT: f64 = 0.1;
const MIN_SHAPE_SIZE_PT: f64 = 0.5;
const ELLIPSE_SEGMENTS: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Eraser,
    Line,
    Rect,
    Ellipse,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Pen => "画笔",
            Tool::Eraser => "橡皮擦",
            Tool::Line => "直线",
            Tool::Rect => "矩形",
            Tool::Ellipse => "椭圆",
        }
    }

    pub fn is_shape(self) -> bool {
        matches!(self, Tool::Line | Tool::Rect | Tool::Ellipse)
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub tool: Tool,
    pub color: Rgba,
    pub width_pt: f64,
    pub pressure_gamma: f64,
    pub min_pressure_ratio: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tool: Tool::Pen,
            color: Rgba::BLACK,
            width_pt: 2.0,
            pressure_gamma: 1.0,
            min_pressure_ratio: 0.25,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeviceStatus {
    pub name: String,
    pub tool: ToolKind,
    pub pressure: f64,
    pub pressure_available: bool,
    pub has_pressure_axis: bool,
}

#[derive(Debug)]
struct FreehandDraft {
    points: Vec<Point>,
    widths: Vec<f64>,
    variable: bool,
    /// 这一笔是否真的收到过压感读数（用于诊断“有压感轴却画成常量宽”）。
    saw_pressure: bool,
    /// 起笔时设备是否带压感轴。
    device_has_pressure_axis: bool,
    last_logical: (f64, f64),
}

#[derive(Debug)]
enum Gesture {
    Idle,
    Freehand(FreehandDraft),
    Erasing { last: Point },
    Shape(ShapeDraft),
}

#[derive(Debug)]
struct ShapeDraft {
    tool: Tool,
    start: Point,
    current: Point,
    shift: bool,
}

pub struct App {
    pub document: Document,
    pub settings: Settings,
    device: Option<DeviceStatus>,
    /// 最近一次读到的有效压感；事件缺少压感轴时沿用它，避免宽度突然跳到满宽。
    last_pressure: Option<f64>,
    gesture: Gesture,
    dirty: bool,
    project_path: Option<PathBuf>,
}

impl App {
    pub fn new(document: Document) -> Self {
        Self {
            document,
            settings: Settings::default(),
            device: None,
            last_pressure: None,
            gesture: Gesture::Idle,
            dirty: false,
            project_path: None,
        }
    }

    pub fn device(&self) -> Option<&DeviceStatus> {
        self.device.as_ref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }

    pub fn set_project_path(&mut self, path: Option<PathBuf>) {
        self.project_path = path;
    }

    pub fn replace_document(&mut self, document: Document, project_path: Option<PathBuf>) {
        self.document = document;
        self.gesture = Gesture::Idle;
        self.dirty = false;
        self.project_path = project_path;
    }

    pub fn is_drawing(&self) -> bool {
        !matches!(self.gesture, Gesture::Idle)
    }

    pub fn handle_input(&mut self, event: InputEvent, view: &ViewTransform) -> bool {
        match event {
            InputEvent::Down(sample) => self.pointer_down(sample, view),
            InputEvent::Move(sample) => self.pointer_move(sample, view),
            InputEvent::Up(sample) => self.pointer_up(sample, view),
            InputEvent::ProximityOut => self.finish_gesture(),
            InputEvent::Cancel => self.cancel_gesture(),
        }
    }

    fn pointer_down(&mut self, sample: PointerSample, view: &ViewTransform) -> bool {
        self.update_device(&sample);
        let point = view.logical_to_page(sample.logical.0, sample.logical.1);
        if !point.is_finite() {
            return false;
        }

        // 在隐藏图层上落笔时自动把该层显示出来，否则笔迹提交后会凭空消失。
        if !self.document.active_layer().visible {
            let active = self.document.active;
            if self.document.set_layer_visible(active, true) {
                log::info!("在隐藏图层上落笔，已自动显示该图层");
            }
        }

        if sample.tool == ToolKind::Eraser || self.settings.tool == Tool::Eraser {
            self.gesture = Gesture::Erasing { last: point };
            let removed = self
                .document
                .erase_segment_active(point, point, ERASE_TOLERANCE_PT);
            if removed > 0 {
                self.dirty = true;
            }
            return removed > 0;
        }

        if self.settings.tool.is_shape() {
            self.gesture = Gesture::Shape(ShapeDraft {
                tool: self.settings.tool,
                start: point,
                current: point,
                shift: sample.shift,
            });
            return true;
        }

        let variable = self.variable_for(&sample);
        let width = effective_width(&self.settings, self.pressure_for(&sample), variable);
        self.gesture = Gesture::Freehand(FreehandDraft {
            points: vec![point],
            widths: vec![width],
            variable,
            saw_pressure: sample.pressure_available,
            device_has_pressure_axis: sample.device_has_pressure,
            last_logical: sample.logical,
        });
        true
    }

    fn pointer_move(&mut self, sample: PointerSample, view: &ViewTransform) -> bool {
        self.update_device(&sample);
        // 先算好，避免在 &mut self.gesture 借用期间再借用整个 self。
        let pressure = self.pressure_for(&sample);
        let can_be_variable = self.variable_for(&sample);
        match &mut self.gesture {
            Gesture::Idle => false,
            Gesture::Freehand(draft) => {
                draft.saw_pressure |= sample.pressure_available;
                let upgraded =
                    upgrade_to_variable(draft, &self.settings, pressure, can_be_variable);
                let (last_x, last_y) = draft.last_logical;
                if (sample.logical.0 - last_x).hypot(sample.logical.1 - last_y)
                    < MIN_SAMPLE_DISTANCE_LOGICAL
                {
                    return upgraded;
                }
                let point = view.logical_to_page(sample.logical.0, sample.logical.1);
                if !point.is_finite() {
                    return upgraded;
                }
                let width = effective_width(&self.settings, pressure, draft.variable);
                draft.points.push(point);
                draft.widths.push(width);
                draft.last_logical = sample.logical;
                true
            }
            Gesture::Erasing { last } => {
                let point = view.logical_to_page(sample.logical.0, sample.logical.1);
                if !point.is_finite() {
                    return false;
                }
                let from = *last;
                *last = point;
                let removed = self
                    .document
                    .erase_segment_active(from, point, ERASE_TOLERANCE_PT);
                if removed > 0 {
                    self.dirty = true;
                }
                removed > 0
            }
            Gesture::Shape(draft) => {
                let point = view.logical_to_page(sample.logical.0, sample.logical.1);
                if !point.is_finite() {
                    return false;
                }
                let changed = draft.current.x != point.x
                    || draft.current.y != point.y
                    || draft.shift != sample.shift;
                draft.current = point;
                draft.shift = sample.shift;
                changed
            }
        }
    }

    fn pointer_up(&mut self, sample: PointerSample, view: &ViewTransform) -> bool {
        self.update_device(&sample);
        let pressure = self.pressure_for(&sample);
        let can_be_variable = self.variable_for(&sample);
        match &mut self.gesture {
            Gesture::Freehand(draft) => {
                draft.saw_pressure |= sample.pressure_available;
                upgrade_to_variable(draft, &self.settings, pressure, can_be_variable);
                let (last_x, last_y) = draft.last_logical;
                if (sample.logical.0 - last_x).hypot(sample.logical.1 - last_y)
                    >= MIN_SAMPLE_DISTANCE_LOGICAL
                {
                    let point = view.logical_to_page(sample.logical.0, sample.logical.1);
                    if point.is_finite() {
                        let width = effective_width(&self.settings, pressure, draft.variable);
                        draft.points.push(point);
                        draft.widths.push(width);
                        draft.last_logical = sample.logical;
                    }
                }
            }
            Gesture::Shape(draft) => {
                let point = view.logical_to_page(sample.logical.0, sample.logical.1);
                if point.is_finite() {
                    draft.current = point;
                }
                draft.shift = sample.shift;
            }
            _ => {}
        }
        self.finish_gesture()
    }

    fn finish_gesture(&mut self) -> bool {
        let gesture = std::mem::replace(&mut self.gesture, Gesture::Idle);
        match gesture {
            Gesture::Freehand(draft) => {
                if draft.points.is_empty() {
                    return false;
                }
                if !draft.variable && draft.device_has_pressure_axis && !draft.saw_pressure {
                    log::warn!(
                        "设备带压感轴，但这一笔始终没有收到压感读数，已按 {:.2}pt 常量宽绘制",
                        self.settings.width_pt
                    );
                }
                let mut points = draft.points;
                let mut widths = draft.widths;
                if points.len() == 1 {
                    points.push(points[0]);
                    widths.push(widths[0]);
                }
                let stroke = if draft.variable {
                    Stroke::variable(self.settings.color, self.settings.width_pt, widths, points)
                } else {
                    Stroke::constant(self.settings.color, self.settings.width_pt, points)
                };
                self.document.active_layer_mut().strokes.push(stroke);
                self.dirty = true;
                true
            }
            Gesture::Shape(draft) => {
                let Some(stroke) = build_shape_stroke(&self.settings, &draft) else {
                    return false;
                };
                self.document.active_layer_mut().strokes.push(stroke);
                self.dirty = true;
                true
            }
            Gesture::Erasing { .. } | Gesture::Idle => false,
        }
    }

    fn cancel_gesture(&mut self) -> bool {
        let had_gesture = self.is_drawing();
        self.gesture = Gesture::Idle;
        had_gesture
    }

    /// 提交正在绘制的手势（保存/导出前调用），返回是否产生了新内容。
    ///
    /// 保存与导出只能看到已提交的笔迹，因此入口处必须先把草稿落到当前层，
    /// 否则“正在画的最后一笔”会丢失。
    pub fn finish_draft(&mut self) -> bool {
        self.finish_gesture()
    }

    /// 当前手势是否在擦除（擦除会整条删笔迹，调用方需整屏重绘）。
    pub fn is_erasing(&self) -> bool {
        matches!(self.gesture, Gesture::Erasing { .. })
    }

    /// 本帧真正新画上去的页面坐标范围 `(x0, y0, x1, y1)`，用于脏矩形重绘。
    ///
    /// - 自由笔迹只返回最后一段：更早的部分在之前的帧里已经画好，不必每帧重刷；
    /// - 图形预览每帧形状都在变，返回整个图形；
    /// - 擦除/空闲返回 `None`，调用方应整屏重绘。
    pub fn draft_damage_pt(&self) -> Option<(f64, f64, f64, f64)> {
        let margin = 2.0;
        match &self.gesture {
            Gesture::Freehand(draft) => {
                let count = draft.points.len();
                if count == 0 {
                    return None;
                }
                let last = draft.points[count - 1];
                let prev = if count >= 2 {
                    draft.points[count - 2]
                } else {
                    last
                };
                let width_last = draft.widths[count - 1];
                let width_prev = if count >= 2 {
                    draft.widths[count - 2]
                } else {
                    width_last
                };
                let half = width_last.max(width_prev).max(0.1) / 2.0;
                Some(bounds_with_margin(&[prev, last], half + margin))
            }
            Gesture::Shape(draft) => {
                let half = self.settings.width_pt.max(0.1) / 2.0 + margin;
                Some(bounds_with_margin(&[draft.start, draft.current], half))
            }
            Gesture::Erasing { .. } | Gesture::Idle => None,
        }
    }

    pub fn draft_stroke(&self) -> Option<Stroke> {
        match &self.gesture {
            Gesture::Freehand(draft) => {
                if draft.points.is_empty() {
                    return None;
                }
                let mut points = draft.points.clone();
                let mut widths = draft.widths.clone();
                if points.len() == 1 {
                    points.push(points[0]);
                    widths.push(widths[0]);
                }
                Some(Stroke {
                    color: self.settings.color,
                    base_width_pt: self.settings.width_pt,
                    widths_pt: if draft.variable { Some(widths) } else { None },
                    points,
                })
            }
            Gesture::Shape(draft) => build_shape_stroke(&self.settings, draft),
            Gesture::Idle | Gesture::Erasing { .. } => None,
        }
    }

    pub fn clear_active_layer(&mut self) -> bool {
        let layer = self.document.active_layer_mut();
        if layer.strokes.is_empty() {
            return false;
        }
        layer.strokes.clear();
        self.dirty = true;
        true
    }

    /// 本事件的压感值：缺少压感轴时沿用上一次读数，避免宽度突然跳到满宽。
    fn pressure_for(&self, sample: &PointerSample) -> f64 {
        if sample.pressure_available {
            sample.pressure
        } else {
            self.last_pressure.unwrap_or(0.5)
        }
    }

    /// 这一笔能否带压感：设备必须有压感轴，且本次或此前读到过有效压感。
    fn variable_for(&self, sample: &PointerSample) -> bool {
        sample.device_has_pressure && (sample.pressure_available || self.last_pressure.is_some())
    }

    fn update_device(&mut self, sample: &PointerSample) {
        if sample.pressure_available {
            self.last_pressure = Some(sample.pressure);
        }
        self.device = Some(DeviceStatus {
            name: sample.device_name.clone(),
            tool: sample.tool,
            pressure: sample.pressure,
            pressure_available: sample.pressure_available,
            has_pressure_axis: sample.device_has_pressure,
        });
    }
}

/// 一组点向外扩张 `margin` 后的包围盒。
fn bounds_with_margin(points: &[Point], margin: f64) -> (f64, f64, f64, f64) {
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for point in points {
        x0 = x0.min(point.x);
        y0 = y0.min(point.y);
        x1 = x1.max(point.x);
        y1 = y1.max(point.y);
    }
    (x0 - margin, y0 - margin, x1 + margin, y1 + margin)
}

/// 起笔事件没带压感轴时整笔会被锁成常量宽（= 满笔宽，明显偏粗）。
/// 一旦后续事件拿到压感，就把已有的点补成压感宽度并切换为变宽笔迹。
/// 返回是否发生了升级（调用方据此决定要不要重绘）。
fn upgrade_to_variable(
    draft: &mut FreehandDraft,
    settings: &Settings,
    pressure: f64,
    can_be_variable: bool,
) -> bool {
    if draft.variable || !can_be_variable {
        return false;
    }
    draft.variable = true;
    let width = effective_width(settings, pressure, true);
    draft.widths.fill(width);
    true
}

fn effective_width(settings: &Settings, pressure: f64, variable: bool) -> f64 {
    if !variable {
        return settings.width_pt.max(MIN_STROKE_WIDTH_PT);
    }
    pressure_width(
        settings.width_pt,
        pressure,
        settings.pressure_gamma,
        settings.min_pressure_ratio,
    )
    .max(MIN_STROKE_WIDTH_PT)
}

fn build_shape_stroke(settings: &Settings, draft: &ShapeDraft) -> Option<Stroke> {
    if !draft.start.is_finite() || !draft.current.is_finite() {
        return None;
    }
    let points = shape_points(draft)?;
    Some(Stroke::constant(settings.color, settings.width_pt, points))
}

fn shape_points(draft: &ShapeDraft) -> Option<Vec<Point>> {
    let start = draft.start;
    let mut dx = draft.current.x - start.x;
    let mut dy = draft.current.y - start.y;
    match draft.tool {
        Tool::Line => {
            if dx.hypot(dy) < MIN_SHAPE_SIZE_PT {
                return None;
            }
            Some(vec![start, draft.current])
        }
        Tool::Rect => {
            if draft.shift {
                let side = dx.abs().min(dy.abs());
                dx = side.copysign(dx);
                dy = side.copysign(dy);
            }
            if dx.abs() < MIN_SHAPE_SIZE_PT || dy.abs() < MIN_SHAPE_SIZE_PT {
                return None;
            }
            let end = Point::new(start.x + dx, start.y + dy);
            Some(vec![
                start,
                Point::new(end.x, start.y),
                end,
                Point::new(start.x, end.y),
                start,
            ])
        }
        Tool::Ellipse => {
            let (mut rx, mut ry) = (dx.abs() / 2.0, dy.abs() / 2.0);
            if draft.shift {
                let radius = rx.min(ry);
                rx = radius;
                ry = radius;
            }
            if rx < MIN_SHAPE_SIZE_PT / 2.0 || ry < MIN_SHAPE_SIZE_PT / 2.0 {
                return None;
            }
            let center = Point::new(start.x + dx / 2.0, start.y + dy / 2.0);
            let mut points = Vec::with_capacity(ELLIPSE_SEGMENTS + 1);
            for index in 0..=ELLIPSE_SEGMENTS {
                let angle = 2.0 * PI * index as f64 / ELLIPSE_SEGMENTS as f64;
                points.push(Point::new(
                    center.x + rx * angle.cos(),
                    center.y + ry * angle.sin(),
                ));
            }
            Some(points)
        }
        Tool::Pen | Tool::Eraser => None,
    }
}
