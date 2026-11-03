//! 画布进程的**无 GUI 控制器**：拥有文档、缓存、视图，并把 IPC 命令翻译成模型操作。
//!
//! 这里刻意不出现任何 GTK 类型：窗口/截图/穿透由 [`CanvasHost`] 注入，
//! 因此可以在无显示环境下用 MockHost 做单元测试。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use base64::Engine;
use link_ipc::{
    BackgroundDto, Command, Event, ImageFormatDto, LayerView, MonitorDto, ToolDto, PROTOCOL_VERSION,
};

use crate::app::{App, Tool};
use crate::cache::OverlayCache;
use crate::export::{self, ExportBackground, ImageFormat};
use crate::input::InputEvent;
use crate::model::Rgba;
use crate::platform::{CaptureImage, MonitorInfo, PlatformError};
use crate::view::ViewTransform;
use crate::xopp;

/// 设备状态事件的最小间隔，避免每个指针事件都跨进程发消息。
const DEVICE_EVENT_INTERVAL: Duration = Duration::from_millis(100);
/// 图层缩略图的像素尺寸（16:9，和页面比例一致）。
const THUMBNAIL_WIDTH: u32 = 128;
const THUMBNAIL_HEIGHT: u32 = 72;

/// 画布进程里所有需要"窗口/系统"的能力。真实实现见 `canvas_main`，测试用 Mock。
pub trait CanvasHost {
    /// 开启/关闭穿透（覆盖层不接收输入）。
    fn set_click_through(&mut self, enabled: bool) -> Result<(), PlatformError>;
    /// 隐藏覆盖层、等待合成器出帧、抓取主屏、恢复覆盖层。
    fn capture_primary_hidden(&mut self) -> Result<CaptureImage, PlatformError>;
}

pub struct CanvasApp {
    app: App,
    view: ViewTransform,
    monitor: MonitorInfo,
    cache: OverlayCache,
    passthrough: bool,
    redraw: bool,
    last_device_text: Option<String>,
    last_device_emit: Option<Instant>,
}

impl CanvasApp {
    pub fn new(monitor: MonitorInfo) -> Self {
        let document = crate::model::Document::new(monitor.width_pt(), monitor.height_pt());
        let view = ViewTransform::for_monitor(&monitor, document.width_pt, document.height_pt);
        Self {
            app: App::new(document),
            view,
            monitor,
            cache: OverlayCache::new(&monitor, monitor.scale),
            passthrough: false,
            redraw: true,
            last_device_text: None,
            last_device_emit: None,
        }
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    pub fn monitor(&self) -> MonitorInfo {
        self.monitor
    }

    pub fn passthrough(&self) -> bool {
        self.passthrough
    }

    pub fn view(&self) -> ViewTransform {
        self.view
    }

    /// 取出"需要重绘"标志（取走即清零）。
    pub fn take_redraw(&mut self) -> bool {
        std::mem::take(&mut self.redraw)
    }

    /// `damage` 与 `clip` 都是**设备像素**（画布后端统一用物理像素坐标）。
    pub fn draw(
        &mut self,
        canvas: &mut dyn crate::paint::Canvas,
        damage: Option<(f64, f64, f64, f64)>,
        clip: crate::paint::RectF,
    ) {
        // 渲染与缓存都按物理像素工作：后端不再持有设备缩放。
        let physical = self.view.to_physical_pixels(self.monitor.scale);
        crate::ui::overlay::draw(
            canvas,
            &self.app,
            &physical,
            &mut self.cache,
            damage,
            clip,
        );
    }

    /// 整屏重绘时要用的裁剪矩形（设备像素）。
    pub fn full_clip(&self) -> crate::paint::RectF {
        let scale = self.monitor.scale.max(0.01);
        crate::paint::RectF::new(
            0.0,
            0.0,
            self.monitor.logical.width * scale,
            self.monitor.logical.height * scale,
        )
    }

    /// 页面 pt 的脏矩形 → 设备像素矩形（窗口层排队重绘用）。
    pub fn damage_to_device(
        &self,
        damage: Option<(f64, f64, f64, f64)>,
    ) -> Option<(f64, f64, f64, f64)> {
        let physical = self.view.to_physical_pixels(self.monitor.scale);
        damage.map(|(x0, y0, x1, y1)| {
            let (ax, ay) = physical.page_to_logical(crate::model::Point::new(x0, y0));
            let (bx, by) = physical.page_to_logical(crate::model::Point::new(x1, y1));
            (ax.min(bx), ay.min(by), ax.max(bx), ay.max(by))
        })
    }

    /// 处理一条输入事件，返回要发给面板的事件。
    pub fn on_input(&mut self, event: InputEvent) -> Vec<Event> {
        let strokes_before = self.stroke_count();
        let selection_before = self.app.selection_len();
        let revision_before = self.app.selection_revision();
        self.redraw |= self.app.handle_input(event, &self.view);
        let revision_changed = self.app.selection_revision() != revision_before;
        if revision_changed {
            self.cache.invalidate();
        }
        let mut events = Vec::new();
        if !self.app.is_selection_transient() {
            let strokes_changed = self.stroke_count() != strokes_before;
            let selection_changed = self.app.selection_len() != selection_before;
            if strokes_changed || selection_changed || revision_changed {
                events.push(self.state_event(strokes_changed || revision_changed));
            }
        }
        if let Some(device) = self.device_event() {
            events.push(device);
        }
        events
    }

    pub fn ready_event(&self) -> Event {
        Event::Ready {
            proto: PROTOCOL_VERSION,
            monitor: MonitorDto {
                x: self.monitor.physical.x,
                y: self.monitor.physical.y,
                width: self.monitor.physical.width,
                height: self.monitor.physical.height,
                logical_width: self.monitor.logical.width,
                logical_height: self.monitor.logical.height,
                scale: self.monitor.scale,
            },
        }
    }

    pub fn state_event(&self, with_thumbnails: bool) -> Event {
        let document = &self.app.document;
        let layers = document
            .layers
            .iter()
            .enumerate()
            .map(|(index, layer)| LayerView {
                id: index,
                name: layer.name.clone(),
                visible: layer.visible,
                thumbnail_png: if with_thumbnails {
                    export::layer_thumbnail_png(
                        layer,
                        document.width_pt,
                        document.height_pt,
                        THUMBNAIL_WIDTH,
                        THUMBNAIL_HEIGHT,
                    )
                    .ok()
                    .map(|png| base64::engine::general_purpose::STANDARD.encode(png))
                } else {
                    None
                },
            })
            .collect();
        Event::State {
            layers,
            active: document.active,
            passthrough: self.passthrough,
            selection: self.app.selection_len(),
        }
    }

    pub fn handle_command(&mut self, command: Command, host: &mut dyn CanvasHost) -> Vec<Event> {
        match command {
            Command::Hello { proto } => {
                if proto != PROTOCOL_VERSION {
                    // 面板与画布是两个可执行文件，很容易一新一旧（cargo run 只重建面板）。
                    return vec![Event::Error {
                        message: format!(
                            "画布与面板版本不一致（面板 {proto}，画布 {PROTOCOL_VERSION}）：\
                             请执行 `cargo build --workspace` 重新构建后再启动"
                        ),
                    }];
                }
                vec![self.ready_event(), self.state_event(true)]
            }
            Command::SetTool { tool } => {
                if self.app.set_tool(tool_from_dto(tool)) {
                    self.redraw = true;
                    vec![self.state_event(false)]
                } else {
                    Vec::new()
                }
            }
            Command::SetColor { rgba } => {
                self.app.settings.color = Rgba::new(rgba[0], rgba[1], rgba[2], rgba[3]);
                Vec::new()
            }
            Command::SetWidth { pt } => {
                self.app.settings.width_pt = pt.clamp(0.1, 64.0);
                Vec::new()
            }
            Command::SetPressure { gamma, min_ratio } => {
                self.app.settings.pressure_gamma = gamma;
                self.app.settings.min_pressure_ratio = min_ratio;
                Vec::new()
            }
            Command::LayerAdd => {
                self.app.clear_selection();
                self.app.document.add_layer_above_active();
                self.mark_dirty_and_redraw();
                vec![self.state_event(true)]
            }
            Command::LayerRemove => {
                let cleared = self.app.clear_selection();
                if self.app.document.remove_active_layer() {
                    self.mark_dirty_and_redraw();
                    vec![self.state_event(true)]
                } else if cleared {
                    self.redraw = true;
                    vec![self.state_event(false)]
                } else {
                    Vec::new()
                }
            }
            Command::LayerMove { up } => {
                let cleared = self.app.clear_selection();
                if self.app.document.move_active_layer(up) {
                    self.mark_dirty_and_redraw();
                    vec![self.state_event(true)]
                } else if cleared {
                    self.redraw = true;
                    vec![self.state_event(false)]
                } else {
                    Vec::new()
                }
            }
            Command::LayerSelect { index } => {
                if index < self.app.document.layers.len() {
                    self.app.document.active = index;
                    self.redraw = true;
                    vec![self.state_event(false)]
                } else {
                    Vec::new()
                }
            }
            Command::LayerRename { index, name } => {
                if self.app.document.rename_layer(index, &name) {
                    self.app.mark_dirty();
                    vec![self.state_event(false)]
                } else {
                    Vec::new()
                }
            }
            Command::LayerVisibility { index, visible } => {
                if self.app.document.set_layer_visible(index, visible) {
                    self.app.clear_selection();
                    self.redraw = true;
                    vec![self.state_event(false)]
                } else {
                    Vec::new()
                }
            }
            Command::Undo => {
                let had_selection = self.app.selection_len() > 0;
                match self.app.undo() {
                    Some(result) => {
                        // 撤回可能恢复一大片被擦掉的笔迹，直接整屏重绘最省心。
                        self.mark_dirty_and_redraw();
                        self.cache.invalidate();
                        log::debug!(
                            "撤回：图层 {}，{}",
                            result.layer,
                            if result.restored {
                                "恢复被删笔迹"
                            } else {
                                "删除最近一笔"
                            }
                        );
                        vec![self.state_event(true)]
                    }
                    None => {
                        log::debug!("撤回：没有可撤回的内容");
                        if had_selection {
                            self.redraw = true;
                            vec![self.state_event(false)]
                        } else {
                            Vec::new()
                        }
                    }
                }
            }
            Command::ClearLayer => {
                if self.app.clear_active_layer() {
                    self.redraw = true;
                    vec![self.state_event(true)]
                } else {
                    Vec::new()
                }
            }
            Command::RecolorSelection { rgba } => {
                let color = Rgba::new(rgba[0], rgba[1], rgba[2], rgba[3]);
                if self.app.recolor_selection(color) {
                    self.mark_dirty_and_redraw();
                    self.cache.invalidate();
                    vec![self.state_event(true)]
                } else {
                    Vec::new()
                }
            }
            Command::ScaleSelection { factor } => {
                if self.app.scale_selection(factor) {
                    self.mark_dirty_and_redraw();
                    self.cache.invalidate();
                    vec![self.state_event(true)]
                } else {
                    Vec::new()
                }
            }
            Command::MoveSelectionToLayer { index } => {
                if self.app.move_selection_to_layer(index) {
                    self.mark_dirty_and_redraw();
                    self.cache.invalidate();
                    vec![self.state_event(true)]
                } else {
                    Vec::new()
                }
            }
            Command::ClearSelection => {
                if self.app.clear_selection() {
                    self.redraw = true;
                    vec![self.state_event(false)]
                } else {
                    Vec::new()
                }
            }
            Command::SetPassthrough { enabled } => match host.set_click_through(enabled) {
                Ok(()) => {
                    self.passthrough = enabled;
                    vec![self.state_event(false)]
                }
                Err(error) => vec![Event::Error {
                    message: format!("切换穿透失败：{error}"),
                }],
            },
            Command::SaveXopp { path } => self.save_xopp(path),
            Command::OpenXopp { path } => self.open_xopp(path),
            Command::ExportImage {
                path,
                format,
                background,
            } => self.export_image(path, format, background, host),
            Command::Quit => Vec::new(),
        }
    }

    fn save_xopp(&mut self, path: String) -> Vec<Event> {
        // 保存只能看到已提交的笔迹：先把草稿落盘。
        if self.app.finish_draft() {
            self.redraw = true;
        }
        let path = PathBuf::from(path);
        match xopp::save(&path, &self.app.document) {
            Ok(()) => {
                self.app.mark_saved();
                self.app.set_project_path(Some(path.clone()));
                vec![Event::Saved {
                    path: path.display().to_string(),
                }]
            }
            Err(error) => vec![Event::Error {
                message: format!("保存工程失败：{error}"),
            }],
        }
    }

    fn open_xopp(&mut self, path: String) -> Vec<Event> {
        let path = PathBuf::from(path);
        match xopp::load(&path) {
            Ok(outcome) => {
                let warnings = outcome.warnings.clone();
                let project_path = if outcome.ignored_unsupported {
                    None
                } else {
                    Some(path.clone())
                };
                let document = outcome.document;
                self.view = ViewTransform::for_monitor(
                    &self.monitor,
                    document.width_pt,
                    document.height_pt,
                );
                self.app.replace_document(document, project_path);
                self.redraw = true;
                vec![
                    Event::Opened {
                        path: path.display().to_string(),
                        warnings,
                    },
                    self.state_event(true),
                ]
            }
            Err(error) => vec![Event::Error {
                message: format!("打开工程失败：{error}"),
            }],
        }
    }

    fn export_image(
        &mut self,
        path: String,
        format: ImageFormatDto,
        background: BackgroundDto,
        host: &mut dyn CanvasHost,
    ) -> Vec<Event> {
        if self.app.finish_draft() {
            self.redraw = true;
        }
        let format = match format {
            ImageFormatDto::Png => ImageFormat::Png,
            ImageFormatDto::Jpeg => ImageFormat::Jpeg,
        };
        let document = self.app.document.clone();
        let rendered = match background {
            BackgroundDto::Blank => {
                export::render(&document, &self.monitor, ExportBackground::Blank)
            }
            BackgroundDto::Screen => match host.capture_primary_hidden() {
                Ok(capture) => {
                    export::render(&document, &self.monitor, ExportBackground::Screen(&capture))
                }
                Err(error) => {
                    return vec![Event::Error {
                        message: format!("截取屏幕失败：{error}"),
                    }]
                }
            },
        };
        let path = PathBuf::from(path);
        match rendered.and_then(|image| export::save(&path, format, &image)) {
            Ok(()) => vec![Event::Exported {
                path: path.display().to_string(),
            }],
            Err(error) => vec![Event::Error {
                message: format!("导出图片失败：{error}"),
            }],
        }
    }

    fn mark_dirty_and_redraw(&mut self) {
        self.app.mark_dirty();
        self.redraw = true;
    }

    fn stroke_count(&self) -> usize {
        self.app
            .document
            .layers
            .iter()
            .map(|layer| layer.strokes.len())
            .sum()
    }

    fn device_event(&mut self) -> Option<Event> {
        let device = self.app.device()?;
        let text = format!(
            "{}|{}|{}|{}",
            device.name,
            device.tool.label(),
            (device.pressure * 20.0).round() as i32,
            device.has_pressure_axis
        );
        let now = Instant::now();
        let changed = self.last_device_text.as_deref() != Some(text.as_str());
        let due = self
            .last_device_emit
            .map(|last| now.duration_since(last) >= DEVICE_EVENT_INTERVAL)
            .unwrap_or(true);
        if !changed || !due {
            return None;
        }
        self.last_device_text = Some(text);
        self.last_device_emit = Some(now);
        Some(Event::Device {
            name: device.name.clone(),
            tool: device.tool.label().to_string(),
            pressure: if device.pressure_available {
                device.pressure
            } else {
                0.0
            },
            has_pressure: device.has_pressure_axis,
        })
    }
}

fn tool_from_dto(tool: ToolDto) -> Tool {
    match tool {
        ToolDto::Pen => Tool::Pen,
        ToolDto::Eraser => Tool::Eraser,
        ToolDto::Line => Tool::Line,
        ToolDto::Rect => Tool::Rect,
        ToolDto::Ellipse => Tool::Ellipse,
        ToolDto::SelectRect => Tool::SelectRect,
        ToolDto::SelectLasso => Tool::SelectLasso,
    }
}
