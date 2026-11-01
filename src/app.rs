use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use crate::input::{pressure_width, InputEvent, PointerSample, ToolKind};
use crate::model::{point_segment_distance, Document, Point, Rgba, Stroke};
use crate::selection::{
    lasso_candidates, normalize_rect, points_bounds, rect_candidates, scaled_stroke, union,
    MarqueeKind, Rect, Selection, SelectionTransform,
};
use crate::undo::{Edit, StrokeChange, UndoResult, UndoStack};
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
    SelectRect,
    SelectLasso,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Pen => "画笔",
            Tool::Eraser => "橡皮擦",
            Tool::Line => "直线",
            Tool::Rect => "矩形",
            Tool::Ellipse => "椭圆",
            Tool::SelectRect => "矩形选择",
            Tool::SelectLasso => "自由选择",
        }
    }

    pub fn is_shape(self) -> bool {
        matches!(self, Tool::Line | Tool::Rect | Tool::Ellipse)
    }

    pub fn is_selection(self) -> bool {
        matches!(self, Tool::SelectRect | Tool::SelectLasso)
    }

    pub fn marquee_kind(self) -> Option<MarqueeKind> {
        match self {
            Tool::SelectRect => Some(MarqueeKind::Rect),
            Tool::SelectLasso => Some(MarqueeKind::Lasso),
            _ => None,
        }
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
    Marquee(MarqueeDraft),
    TransformSelection(TransformDraft),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransformMode {
    Move,
    Scale,
    Rotate,
}

#[derive(Debug)]
struct MarqueeDraft {
    kind: MarqueeKind,
    start: Point,
    current: Point,
    points: Vec<Point>,
    candidates: Vec<(u64, usize)>,
    last_bounds: Option<Rect>,
    damage: Option<Rect>,
}

#[derive(Debug)]
struct TransformDraft {
    mode: TransformMode,
    center: Point,
    last: Point,
    start_distance: f64,
    start_angle: f64,
    offset: (f64, f64),
    factor: f64,
    radians: f64,
    lifted: bool,
    last_preview: Option<Rect>,
    damage: Option<Rect>,
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
    /// 可撤回操作的栈（最多 50 步）。打开工程或换文档时清空。
    undo: UndoStack,
    selection: Selection,
    /// 文档被选区操作修改时递增；画布据此失效缓存。
    selection_revision: u64,
    /// 选区/结构变化后需要整屏重绘一次。
    force_full_redraw: bool,
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
            undo: UndoStack::new(),
            selection: Selection::default(),
            selection_revision: 0,
            force_full_redraw: false,
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
        // 换文档后旧的撤回记录指向的笔迹已经不存在了。
        self.undo.clear();
        self.selection.clear();
        self.selection_revision += 1;
        self.force_full_redraw = true;
    }

    /// 可撤回的操作数。
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
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

        if self.settings.tool.is_selection() {
            let handle_tolerance = 6.0 / view.pt_to_logical().max(1e-6);
            return self.begin_selection(point, handle_tolerance);
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
            return self.erase_at(point, point);
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
        if self.settings.tool.is_selection() {
            let point = view.logical_to_page(sample.logical.0, sample.logical.1);
            if !point.is_finite() {
                return false;
            }
            return self.update_selection_gesture(point);
        }
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
                self.erase_at(from, point)
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
            Gesture::Marquee(_) | Gesture::TransformSelection(_) => false,
        }
    }

    fn pointer_up(&mut self, sample: PointerSample, view: &ViewTransform) -> bool {
        self.update_device(&sample);
        if self.settings.tool.is_selection() {
            let point = view.logical_to_page(sample.logical.0, sample.logical.1);
            if point.is_finite() {
                self.update_selection_gesture(point);
            }
            return self.finish_gesture();
        }
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
                self.push_stroke(stroke);
                true
            }
            Gesture::Shape(draft) => {
                let Some(stroke) = build_shape_stroke(&self.settings, &draft) else {
                    return false;
                };
                self.push_stroke(stroke);
                true
            }
            Gesture::Marquee(draft) => self.finish_marquee(draft),
            Gesture::TransformSelection(draft) => self.commit_selection_transform(draft),
            Gesture::Erasing { .. } | Gesture::Idle => false,
        }
    }

    fn cancel_gesture(&mut self) -> bool {
        let had_gesture = self.is_drawing();
        let lifted = matches!(
            self.gesture,
            Gesture::TransformSelection(TransformDraft { lifted: true, .. })
        );
        self.gesture = Gesture::Idle;
        if lifted {
            self.restore_lifted_selection();
        }
        had_gesture
    }

    fn begin_selection(&mut self, point: Point, handle_tolerance: f64) -> bool {
        if let Some(bounds) = self.selection.bounds_pt() {
            let center = Point::new((bounds.0 + bounds.2) / 2.0, (bounds.1 + bounds.3) / 2.0);
            let mode = match selection_zone(bounds, point, handle_tolerance) {
                SelectionZone::Corner => Some(TransformMode::Rotate),
                SelectionZone::Edge => Some(TransformMode::Scale),
                SelectionZone::Inside => Some(TransformMode::Move),
                SelectionZone::Outside => None,
            };
            if let Some(mode) = mode {
                let start_distance = point.distance(center).max(1e-3);
                let start_angle = (point.y - center.y).atan2(point.x - center.x);
                self.gesture = Gesture::TransformSelection(TransformDraft {
                    mode,
                    center,
                    last: point,
                    start_distance,
                    start_angle,
                    offset: (0.0, 0.0),
                    factor: 1.0,
                    radians: 0.0,
                    lifted: false,
                    last_preview: Some(expand_rect(bounds)),
                    damage: None,
                });
                return true;
            }
        }
        self.clear_selection();
        let kind = self
            .settings
            .tool
            .marquee_kind()
            .unwrap_or(MarqueeKind::Rect);
        let points = if kind == MarqueeKind::Lasso {
            vec![point]
        } else {
            Vec::new()
        };
        let bounds = expand_rect((point.x, point.y, point.x, point.y));
        self.gesture = Gesture::Marquee(MarqueeDraft {
            kind,
            start: point,
            current: point,
            points,
            candidates: Vec::new(),
            last_bounds: Some(bounds),
            damage: None,
        });
        true
    }

    fn update_selection_gesture(&mut self, point: Point) -> bool {
        let document = &self.document;
        let bounds = self.selection.bounds_pt();
        let page = (document.width_pt, document.height_pt);
        let mut should_lift = false;
        let changed = match &mut self.gesture {
            Gesture::Marquee(draft) => {
                let previous = draft.last_bounds;
                if draft.kind == MarqueeKind::Lasso {
                    let far_enough = draft
                        .points
                        .last()
                        .map(|last| last.distance(point) >= 0.5)
                        .unwrap_or(true);
                    if far_enough {
                        draft.points.push(point);
                    }
                }
                draft.current = point;
                let current = marquee_bounds(draft);
                draft.damage = merge_rect(previous, Some(current));
                draft.last_bounds = Some(current);
                draft.candidates = match draft.kind {
                    MarqueeKind::Rect => {
                        rect_candidates(document, (draft.start.x, draft.start.y, point.x, point.y))
                    }
                    MarqueeKind::Lasso => lasso_candidates(document, &draft.points),
                };
                true
            }
            Gesture::TransformSelection(draft) => {
                let dx = point.x - draft.last.x;
                let dy = point.y - draft.last.y;
                if dx == 0.0 && dy == 0.0 {
                    return false;
                }
                draft.last = point;
                let preview = match draft.mode {
                    TransformMode::Move => {
                        let offset =
                            clamp_offset(bounds, (draft.offset.0 + dx, draft.offset.1 + dy), page);
                        draft.offset = offset;
                        if !draft.lifted && offset.0.hypot(offset.1) > 0.5 {
                            should_lift = true;
                        }
                        bounds.map(|rect| {
                            expand_rect((
                                rect.0 + offset.0,
                                rect.1 + offset.1,
                                rect.2 + offset.0,
                                rect.3 + offset.1,
                            ))
                        })
                    }
                    TransformMode::Scale => {
                        let distance = point.distance(draft.center);
                        draft.factor = (distance / draft.start_distance).clamp(0.05, 20.0);
                        if !draft.lifted && (draft.factor - 1.0).abs() > 0.01 {
                            should_lift = true;
                        }
                        bounds.map(|rect| expand_rect(scale_rect(rect, draft.center, draft.factor)))
                    }
                    TransformMode::Rotate => {
                        draft.radians = (point.y - draft.center.y).atan2(point.x - draft.center.x)
                            - draft.start_angle;
                        if !draft.lifted && draft.radians.abs() > 0.01 {
                            should_lift = true;
                        }
                        bounds.map(|rect| {
                            expand_rect(rotate_rect_bounds(rect, draft.center, draft.radians))
                        })
                    }
                };
                draft.damage = merge_rect(draft.last_preview, preview);
                draft.last_preview = preview;
                true
            }
            _ => false,
        };
        if should_lift {
            self.lift_selection();
            if let Gesture::TransformSelection(draft) = &mut self.gesture {
                draft.lifted = true;
            }
        }
        changed
    }

    fn finish_marquee(&mut self, draft: MarqueeDraft) -> bool {
        self.selection = if marquee_too_small(&draft) {
            Selection::default()
        } else {
            Selection::capture(&self.document, &draft.candidates)
        };
        self.force_full_redraw = true;
        !self.selection.is_empty()
    }

    fn lift_selection(&mut self) {
        let mut removals: Vec<(u64, usize)> = self
            .selection
            .items()
            .iter()
            .map(|item| (item.layer_id, item.index))
            .collect();
        removals.sort_by_key(|(_, index)| std::cmp::Reverse(*index));
        for (layer_id, stroke_index) in removals {
            if let Some(layer_index) = self.document.layer_index_by_id(layer_id) {
                let strokes = &mut self.document.layers[layer_index].strokes;
                if stroke_index < strokes.len() {
                    strokes.remove(stroke_index);
                }
            }
        }
        self.selection_revision += 1;
        self.force_full_redraw = true;
    }

    fn restore_lifted_selection(&mut self) {
        let mut items: Vec<(u64, usize, Stroke)> = self
            .selection
            .items()
            .iter()
            .map(|item| (item.layer_id, item.index, item.stroke.clone()))
            .collect();
        items.sort_by_key(|(_, index, _)| *index);
        for (layer_id, stroke_index, stroke) in items {
            if let Some(layer_index) = self.document.layer_index_by_id(layer_id) {
                let strokes = &mut self.document.layers[layer_index].strokes;
                let position = stroke_index.min(strokes.len());
                strokes.insert(position, stroke);
            }
        }
        self.selection_revision += 1;
        self.force_full_redraw = true;
    }

    fn commit_selection_transform(&mut self, draft: TransformDraft) -> bool {
        if !draft.lifted {
            return false;
        }
        let before_items: Vec<(u64, usize, Stroke)> = self
            .selection
            .items()
            .iter()
            .map(|item| (item.layer_id, item.index, item.stroke.clone()))
            .collect();
        let changed = match draft.mode {
            TransformMode::Move => {
                self.selection.translate(draft.offset.0, draft.offset.1);
                draft.offset.0.abs() > 1e-6 || draft.offset.1.abs() > 1e-6
            }
            TransformMode::Scale => {
                self.selection.scale(draft.center, draft.factor);
                (draft.factor - 1.0).abs() > 1e-6
            }
            TransformMode::Rotate => {
                self.selection.rotate(draft.center, draft.radians);
                draft.radians.abs() > 1e-6
            }
        };
        let mut items: Vec<(u64, usize, Stroke)> = self
            .selection
            .items()
            .iter()
            .map(|item| (item.layer_id, item.index, item.stroke.clone()))
            .collect();
        items.sort_by_key(|(_, index, _)| *index);
        for (layer_id, stroke_index, stroke) in items {
            if let Some(layer_index) = self.document.layer_index_by_id(layer_id) {
                let strokes = &mut self.document.layers[layer_index].strokes;
                let position = stroke_index.min(strokes.len());
                strokes.insert(position, stroke);
            }
        }
        if changed {
            let changes = before_items
                .iter()
                .zip(self.selection.items())
                .map(|((layer, index, before), item)| StrokeChange {
                    before_layer: *layer,
                    before_index: *index,
                    before: before.clone(),
                    after_layer: item.layer_id,
                    after_index: item.index,
                    after: item.stroke.clone(),
                })
                .collect();
            self.undo.record(Edit::Modified { changes });
            self.dirty = true;
        }
        self.selection_revision += 1;
        self.force_full_redraw = true;
        true
    }

    /// 提交正在绘制的手势（保存/导出前调用），返回是否产生了新内容。
    ///
    /// 保存与导出只能看到已提交的笔迹，因此入口处必须先把草稿落到当前层，
    /// 否则“正在画的最后一笔”会丢失。
    pub fn finish_draft(&mut self) -> bool {
        self.finish_gesture()
    }

    pub fn set_tool(&mut self, tool: Tool) -> bool {
        if self.settings.tool == tool {
            return false;
        }
        let had_gesture = self.is_drawing();
        if had_gesture {
            self.cancel_gesture();
        }
        self.settings.tool = tool;
        let cleared = self.clear_selection();
        had_gesture || cleared
    }

    pub fn selection_len(&self) -> usize {
        self.selection.len()
    }

    /// 正在拖动选区（笔迹已从文档临时提起）时，面板不需要收到中间状态。
    pub fn is_selection_transient(&self) -> bool {
        matches!(&self.gesture, Gesture::TransformSelection(draft) if draft.lifted)
    }

    pub fn selection_revision(&self) -> u64 {
        self.selection_revision
    }

    pub fn take_force_full_redraw(&mut self) -> bool {
        std::mem::take(&mut self.force_full_redraw)
    }

    pub fn clear_selection(&mut self) -> bool {
        if self.selection.is_empty() {
            return false;
        }
        self.selection.clear();
        self.force_full_redraw = true;
        true
    }

    pub fn selection_bounds(&self) -> Option<Rect> {
        self.selection.bounds_pt()
    }

    pub fn selection_transform(&self) -> SelectionTransform {
        match &self.gesture {
            Gesture::TransformSelection(draft) => match draft.mode {
                TransformMode::Move => SelectionTransform::Move {
                    dx: draft.offset.0,
                    dy: draft.offset.1,
                },
                TransformMode::Scale => SelectionTransform::Scale {
                    center: draft.center,
                    factor: draft.factor,
                },
                TransformMode::Rotate => SelectionTransform::Rotate {
                    center: draft.center,
                    radians: draft.radians,
                },
            },
            _ => SelectionTransform::None,
        }
    }

    /// 选区矩形框；框选过程中不显示，提交后用细实线显示。
    pub fn selection_box(&self) -> Option<Rect> {
        if matches!(self.gesture, Gesture::Marquee(_)) {
            None
        } else {
            self.selection.bounds_pt()
        }
    }

    pub fn marquee_view(&self) -> Option<(MarqueeKind, Point, Point, &[Point])> {
        match &self.gesture {
            Gesture::Marquee(draft) => {
                Some((draft.kind, draft.start, draft.current, &draft.points))
            }
            _ => None,
        }
    }

    pub fn overlay_strokes(&self) -> Vec<&Stroke> {
        match &self.gesture {
            Gesture::Marquee(draft) => draft
                .candidates
                .iter()
                .filter_map(|(layer_id, index)| {
                    self.document
                        .layers
                        .iter()
                        .find(|layer| layer.id == *layer_id)?
                        .strokes
                        .get(*index)
                })
                .collect(),
            _ => self
                .selection
                .items()
                .iter()
                .map(|item| &item.stroke)
                .collect(),
        }
    }

    pub fn interaction_damage_pt(&self) -> Option<Rect> {
        match &self.gesture {
            Gesture::Marquee(draft) => draft.damage.or(draft.last_bounds),
            Gesture::TransformSelection(draft) => draft.damage,
            _ => self.draft_damage_pt(),
        }
    }

    pub fn recolor_selection(&mut self, color: Rgba) -> bool {
        let mut changes = Vec::new();
        for index in 0..self.selection.len() {
            let (layer_id, stroke_index, before) = {
                let item = &self.selection.items()[index];
                (item.layer_id, item.index, item.stroke.clone())
            };
            let Some(layer_index) = self.document.layer_index_by_id(layer_id) else {
                continue;
            };
            let strokes = &mut self.document.layers[layer_index].strokes;
            if strokes.get(stroke_index) != Some(&before) {
                continue;
            }
            let mut after = before.clone();
            after.color = color;
            strokes[stroke_index] = after.clone();
            self.selection.items_mut()[index].stroke = after.clone();
            changes.push(StrokeChange {
                before_layer: layer_id,
                before_index: stroke_index,
                before,
                after_layer: layer_id,
                after_index: stroke_index,
                after,
            });
        }
        if changes.is_empty() {
            return false;
        }
        self.undo.record(Edit::Modified { changes });
        self.dirty = true;
        self.selection_revision += 1;
        self.force_full_redraw = true;
        true
    }

    pub fn scale_selection(&mut self, factor: f64) -> bool {
        if !factor.is_finite() || self.selection.is_empty() {
            return false;
        }
        let factor = factor.clamp(0.1, 10.0);
        if (factor - 1.0).abs() < 1e-6 {
            return false;
        }
        let Some(bounds) = self.selection.bounds_pt() else {
            return false;
        };
        let center = Point::new((bounds.0 + bounds.2) / 2.0, (bounds.1 + bounds.3) / 2.0);
        let mut changes = Vec::new();
        for index in 0..self.selection.len() {
            let (layer_id, stroke_index, before) = {
                let item = &self.selection.items()[index];
                (item.layer_id, item.index, item.stroke.clone())
            };
            let Some(layer_index) = self.document.layer_index_by_id(layer_id) else {
                continue;
            };
            let strokes = &mut self.document.layers[layer_index].strokes;
            if strokes.get(stroke_index) != Some(&before) {
                continue;
            }
            let after = scaled_stroke(&before, center, factor);
            strokes[stroke_index] = after.clone();
            self.selection.items_mut()[index].stroke = after.clone();
            changes.push(StrokeChange {
                before_layer: layer_id,
                before_index: stroke_index,
                before,
                after_layer: layer_id,
                after_index: stroke_index,
                after,
            });
        }
        if changes.is_empty() {
            return false;
        }
        self.undo.record(Edit::Modified { changes });
        self.dirty = true;
        self.selection_revision += 1;
        self.force_full_redraw = true;
        true
    }

    pub fn move_selection_to_layer(&mut self, index: usize) -> bool {
        if self.selection.is_empty() {
            return false;
        }
        let Some(target) = self.document.layers.get(index) else {
            return false;
        };
        let target_id = target.id;
        if self
            .selection
            .items()
            .iter()
            .all(|item| item.layer_id == target_id)
        {
            return false;
        }
        let before_items: Vec<(u64, usize, Stroke)> = self
            .selection
            .items()
            .iter()
            .map(|item| (item.layer_id, item.index, item.stroke.clone()))
            .collect();

        let mut removals: Vec<(u64, usize)> = before_items
            .iter()
            .map(|(layer, index, _)| (*layer, *index))
            .collect();
        removals.sort_by_key(|(_, index)| std::cmp::Reverse(*index));
        for (layer_id, stroke_index) in removals {
            if let Some(layer_index) = self.document.layer_index_by_id(layer_id) {
                let strokes = &mut self.document.layers[layer_index].strokes;
                if stroke_index < strokes.len() {
                    strokes.remove(stroke_index);
                }
            }
        }

        let Some(target_index) = self.document.layer_index_by_id(target_id) else {
            return false;
        };
        self.document.layers[target_index].visible = true;
        let mut after_indices = Vec::with_capacity(before_items.len());
        let mut changes = Vec::with_capacity(before_items.len());
        for (item_index, (before_layer, before_index, before)) in before_items.iter().enumerate() {
            let after_index = self.document.layers[target_index].strokes.len();
            self.document.layers[target_index]
                .strokes
                .push(before.clone());
            after_indices.push(after_index);
            changes.push(StrokeChange {
                before_layer: *before_layer,
                before_index: *before_index,
                before: before.clone(),
                after_layer: target_id,
                after_index,
                after: before.clone(),
            });
            let _ = item_index;
        }
        let items = self.selection.items_mut();
        for (item, after_index) in items.iter_mut().zip(after_indices) {
            item.layer_id = target_id;
            item.index = after_index;
        }
        self.undo.record(Edit::Modified { changes });
        self.dirty = true;
        self.selection_revision += 1;
        self.force_full_redraw = true;
        true
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
            Gesture::Erasing { .. }
            | Gesture::Idle
            | Gesture::Marquee(_)
            | Gesture::TransformSelection(_) => None,
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
            Gesture::Idle
            | Gesture::Erasing { .. }
            | Gesture::Marquee(_)
            | Gesture::TransformSelection(_) => None,
        }
    }

    pub fn clear_active_layer(&mut self) -> bool {
        let had_selection = self.clear_selection();
        let layer_index = self.document.active;
        if self.document.layers[layer_index].strokes.is_empty() {
            return had_selection;
        }
        let layer_id = self.document.layers[layer_index].id;
        let removed: Vec<(usize, Stroke)> =
            std::mem::take(&mut self.document.layers[layer_index].strokes)
                .into_iter()
                .enumerate()
                .collect();
        self.dirty = true;
        self.undo.record(Edit::Removed {
            layer: layer_id,
            strokes: removed,
        });
        true
    }

    /// 把一笔加到当前图层，并记入撤回栈。
    fn push_stroke(&mut self, stroke: Stroke) {
        let layer_index = self.document.active;
        let layer_id = self.document.layers[layer_index].id;
        let index = self.document.layers[layer_index].strokes.len();
        self.document.layers[layer_index]
            .strokes
            .push(stroke.clone());
        self.dirty = true;
        self.undo.record(Edit::Added {
            layer: layer_id,
            stroke,
            index,
        });
    }

    /// 擦除一段并记入撤回栈，返回是否擦掉了东西。
    fn erase_at(&mut self, from: Point, to: Point) -> bool {
        let layer_id = self.document.active_layer().id;
        let removed = self
            .document
            .erase_segment_active_recorded(from, to, ERASE_TOLERANCE_PT);
        if removed.is_empty() {
            return false;
        }
        self.undo.record(Edit::Removed {
            layer: layer_id,
            strokes: removed,
        });
        self.dirty = true;
        true
    }

    /// 撤回一步（Ctrl+Z）。返回被改动的图层索引，`None` 表示没有可撤回的内容。
    ///
    /// 画到一半时按 Ctrl+Z 只丢弃当前草稿、不撤上一笔，避免"手滑撤掉整笔"。
    pub fn undo(&mut self) -> Option<UndoResult> {
        if self.is_drawing() {
            self.cancel_gesture();
            return None;
        }
        self.clear_selection();
        let result = self.undo.undo(&mut self.document)?;
        self.dirty = true;
        self.force_full_redraw = true;
        Some(result)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionZone {
    Corner,
    Edge,
    Inside,
    Outside,
}

fn selection_zone(bounds: Rect, point: Point, tolerance: f64) -> SelectionZone {
    let bounds = normalize_rect(bounds);
    let corners = [
        Point::new(bounds.0, bounds.1),
        Point::new(bounds.2, bounds.1),
        Point::new(bounds.2, bounds.3),
        Point::new(bounds.0, bounds.3),
    ];
    if corners
        .iter()
        .any(|corner| corner.distance(point) <= tolerance * 1.5)
    {
        return SelectionZone::Corner;
    }
    for index in 0..4 {
        let start = corners[index];
        let end = corners[(index + 1) % 4];
        if point_segment_distance(point, start, end) <= tolerance {
            return SelectionZone::Edge;
        }
    }
    if point.x >= bounds.0 && point.x <= bounds.2 && point.y >= bounds.1 && point.y <= bounds.3 {
        SelectionZone::Inside
    } else {
        SelectionZone::Outside
    }
}

fn scale_rect(rect: Rect, center: Point, factor: f64) -> Rect {
    let rect = normalize_rect(rect);
    (
        center.x + (rect.0 - center.x) * factor,
        center.y + (rect.1 - center.y) * factor,
        center.x + (rect.2 - center.x) * factor,
        center.y + (rect.3 - center.y) * factor,
    )
}

fn rotate_rect_bounds(rect: Rect, center: Point, radians: f64) -> Rect {
    let rect = normalize_rect(rect);
    let (sin, cos) = radians.sin_cos();
    let corners = [
        Point::new(rect.0, rect.1),
        Point::new(rect.2, rect.1),
        Point::new(rect.2, rect.3),
        Point::new(rect.0, rect.3),
    ];
    let rotated: Vec<Point> = corners
        .iter()
        .map(|point| {
            let dx = point.x - center.x;
            let dy = point.y - center.y;
            Point::new(
                center.x + dx * cos - dy * sin,
                center.y + dx * sin + dy * cos,
            )
        })
        .collect();
    points_bounds(&rotated).unwrap_or(rect)
}

fn expand_rect(rect: Rect) -> Rect {
    (rect.0 - 4.0, rect.1 - 4.0, rect.2 + 4.0, rect.3 + 4.0)
}

fn merge_rect(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(union(a, b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn marquee_bounds(draft: &MarqueeDraft) -> Rect {
    let raw = match draft.kind {
        MarqueeKind::Rect => normalize_rect((
            draft.start.x,
            draft.start.y,
            draft.current.x,
            draft.current.y,
        )),
        MarqueeKind::Lasso => {
            let mut bounds = points_bounds(&draft.points).unwrap_or((
                draft.start.x,
                draft.start.y,
                draft.start.x,
                draft.start.y,
            ));
            bounds = union(
                bounds,
                (
                    draft.current.x,
                    draft.current.y,
                    draft.current.x,
                    draft.current.y,
                ),
            );
            bounds
        }
    };
    expand_rect(raw)
}

fn marquee_too_small(draft: &MarqueeDraft) -> bool {
    let raw = match draft.kind {
        MarqueeKind::Rect => normalize_rect((
            draft.start.x,
            draft.start.y,
            draft.current.x,
            draft.current.y,
        )),
        MarqueeKind::Lasso => points_bounds(&draft.points).unwrap_or((
            draft.start.x,
            draft.start.y,
            draft.start.x,
            draft.start.y,
        )),
    };
    raw.2 - raw.0 < 2.0 || raw.3 - raw.1 < 2.0
}

fn clamp_offset(bounds: Option<Rect>, offset: (f64, f64), page: (f64, f64)) -> (f64, f64) {
    let Some(bounds) = bounds else {
        return offset;
    };
    let bounds = normalize_rect(bounds);
    (
        clamp_axis(bounds.0, bounds.2, page.0, offset.0),
        clamp_axis(bounds.1, bounds.3, page.1, offset.1),
    )
}

fn clamp_axis(min: f64, max: f64, total: f64, offset: f64) -> f64 {
    let low = -min;
    let high = total - max;
    if low <= high {
        offset.clamp(low, high)
    } else {
        (total - (max - min)) / 2.0 - min
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
        Tool::Pen | Tool::Eraser | Tool::SelectRect | Tool::SelectLasso => None,
    }
}
