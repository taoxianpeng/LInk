//! LInk 面板（egui）：纯 Rust、无 webview 的控制面板。
//!
//! 画布是一个独立进程（`link-ink-canvas`），本进程只负责界面和命令下发，
//! 两者用 `link-ipc` 的 JSON lines 协议通信。

mod dialog;
mod glyphs;
mod ipc;
mod theme;

use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2,
};
use link_ipc::{BackgroundDto, Command, Event, ImageFormatDto, ToolDto};

use crate::glyphs::Glyph;
use crate::theme::*;

const TOOL_BUTTON: f32 = 42.0;
const SWATCH: f32 = 22.0;

struct LayerUi {
    id: usize,
    name: String,
    visible: bool,
    thumbnail: Option<egui::TextureHandle>,
    thumbnail_b64: Option<String>,
}

#[derive(Debug)]
enum Action {
    SetTool(ToolDto),
    SetColor([u8; 4]),
    SetWidth(f64),
    SetPressure,
    AddLayer,
    RemoveLayer,
    MoveLayer(bool),
    SelectLayer(usize),
    RenameLayer(usize, String),
    ToggleLayer(usize, bool),
    ClearLayer,
    Passthrough(bool),
    Open,
    Save,
    Export,
    ExportTo {
        format: ImageFormatDto,
        background: BackgroundDto,
    },
    BeginRename(usize, String),
    Preview(usize),
    Quit,
}

#[derive(Clone, Debug)]
enum PickKind {
    OpenXopp,
    SaveXopp,
    ExportImage {
        format: ImageFormatDto,
        background: BackgroundDto,
    },
}

struct PendingPick {
    picker: dialog::Picker,
    kind: PickKind,
}

struct PanelApp {
    client: Option<ipc::CanvasClient>,
    startup_error: Option<String>,

    layers: Vec<LayerUi>,
    active: usize,
    passthrough: bool,

    tool: ToolDto,
    color: [u8; 4],
    width: f64,
    gamma: f64,
    min_ratio: f64,

    device: Option<(String, String, f64, bool)>,
    error: Option<(String, Instant)>,

    rename: Option<(usize, String)>,
    preview: Option<usize>,
    show_export: bool,
    export_format: ImageFormatDto,
    export_screen: bool,

    ready: bool,
    start: Instant,
    list_height: f32,
    pending: Option<PendingPick>,
    /// 需要把面板提到画布之上（画布是全屏置顶窗口，会盖住面板）。
    raise_at: Option<Instant>,
    /// 本面板窗口的 X11 window id（仅 Linux 使用）。
    x11_window: Option<u32>,
}

impl PanelApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        install_style(&cc.egui_ctx);

        let (client, startup_error) = match ipc::CanvasClient::spawn(cc.egui_ctx.clone()) {
            Ok(client) => (Some(client), None),
            Err(error) => (None, Some(error)),
        };

        Self {
            client,
            startup_error,
            layers: Vec::new(),
            active: 0,
            passthrough: false,
            tool: ToolDto::Pen,
            color: [0x1C, 0x1C, 0x22, 0xFF],
            width: 2.0,
            gamma: 1.0,
            min_ratio: 0.25,
            device: None,
            error: None,
            rename: None,
            preview: None,
            show_export: false,
            export_format: ImageFormatDto::Png,
            export_screen: false,
            ready: false,
            start: Instant::now(),
            list_height: 180.0,
            pending: None,
            raise_at: None,
            x11_window: x11_window_id(cc),
        }
    }

    fn dispatch(&mut self, ctx: &egui::Context, action: Action) {
        log::debug!("面板动作 -> {action:?}");
        match action {
            Action::SetTool(tool) => {
                self.tool = tool;
                self.send(Command::SetTool { tool });
            }
            Action::SetColor(rgba) => {
                self.color = rgba;
                self.send(Command::SetColor { rgba });
            }
            Action::SetWidth(pt) => {
                self.width = pt;
                self.send(Command::SetWidth { pt });
            }
            Action::SetPressure => {
                self.send(Command::SetPressure {
                    gamma: self.gamma,
                    min_ratio: self.min_ratio,
                });
            }
            Action::AddLayer => self.send(Command::LayerAdd),
            Action::RemoveLayer => self.send(Command::LayerRemove),
            Action::MoveLayer(up) => self.send(Command::LayerMove { up }),
            Action::SelectLayer(index) => self.send(Command::LayerSelect { index }),
            Action::RenameLayer(index, name) => self.send(Command::LayerRename { index, name }),
            Action::ToggleLayer(index, visible) => {
                self.send(Command::LayerVisibility { index, visible })
            }
            Action::ClearLayer => self.send(Command::ClearLayer),
            Action::Passthrough(enabled) => self.send(Command::SetPassthrough { enabled }),
            Action::Open => self.start_pick(ctx, PickKind::OpenXopp),
            Action::Save => self.start_pick(ctx, PickKind::SaveXopp),
            Action::Export => {
                self.show_export = true;
            }
            Action::ExportTo { format, background } => {
                self.start_pick(ctx, PickKind::ExportImage { format, background })
            }
            Action::BeginRename(index, name) => {
                self.rename = Some((index, name));
            }
            Action::Preview(id) => {
                self.preview = if self.preview == Some(id) {
                    None
                } else {
                    Some(id)
                };
            }
            Action::Quit => {
                self.send(Command::Quit);
                std::process::exit(0);
            }
        }
    }

    /// 在工作线程里弹对话框；UI 继续跑，选完再发命令。
    fn start_pick(&mut self, ctx: &egui::Context, kind: PickKind) {
        let request = match &kind {
            PickKind::OpenXopp => dialog::Request::OpenXopp,
            PickKind::SaveXopp => dialog::Request::SaveXopp,
            PickKind::ExportImage { format, .. } => dialog::Request::SaveImage {
                extension: match format {
                    ImageFormatDto::Png => "png",
                    ImageFormatDto::Jpeg => "jpg",
                },
            },
        };
        self.pending = Some(PendingPick {
            picker: dialog::Picker::start(ctx.clone(), request),
            kind,
        });
    }

    fn poll_pick(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        let Some(result) = pending.picker.poll() else {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        };
        let kind = pending.kind.clone();
        self.pending = None;
        let Some(path) = result else {
            log::info!("文件选择已取消");
            return;
        };
        let path = path.display().to_string();
        log::info!("选择文件：{path}");
        self.send(match kind {
            PickKind::OpenXopp => Command::OpenXopp { path },
            PickKind::SaveXopp => Command::SaveXopp { path },
            PickKind::ExportImage { format, background } => Command::ExportImage {
                path,
                format,
                background,
            },
        });
    }

    fn send(&mut self, command: Command) {
        if let Some(client) = self.client.as_mut() {
            client.send(command);
        }
    }

    fn pump(&mut self, ctx: &egui::Context) {
        let events = match self.client.as_mut() {
            Some(client) => client.poll(),
            None => Vec::new(),
        };
        for event in events {
            match event {
                Event::Ready { proto, monitor } => {
                    self.ready = true;
                    log::info!(
                        "画布就绪：协议 {proto}，显示器 {}x{}",
                        monitor.width,
                        monitor.height
                    );
                    let elapsed = self.start.elapsed();
                    log::info!("面板冷启动 -> 画布就绪: {:?}", elapsed);
                    self.raise_at = Some(Instant::now() + Duration::from_millis(250));
                }
                Event::State {
                    layers,
                    active,
                    passthrough,
                } => {
                    self.passthrough = passthrough;
                    self.active = active;
                    self.merge_layers(ctx, layers);
                }
                Event::Device {
                    name,
                    tool,
                    pressure,
                    has_pressure,
                } => {
                    self.device = Some((name, tool, pressure, has_pressure));
                }
                Event::Exported { path } => {
                    self.error = None;
                    log::info!("导出完成: {path}");
                    self.toast(ctx, format!("已导出：{path}"));
                    self.raise_at = Some(Instant::now() + Duration::from_millis(120));
                }
                Event::Saved { path } => {
                    self.toast(ctx, format!("已保存：{path}"));
                }
                Event::Opened { path, warnings } => {
                    let message = if warnings.is_empty() {
                        format!("已打开：{path}")
                    } else {
                        format!("已打开：{path}（{} 条提示）", warnings.len())
                    };
                    self.toast(ctx, message);
                }
                Event::Error { message } => {
                    self.toast(ctx, message);
                }
            }
        }
        if let Some(client) = self.client.as_mut() {
            if let Some(reason) = client.exited_reason.clone() {
                self.startup_error = Some(reason);
                self.client = None;
            }
        }
    }

    fn merge_layers(&mut self, ctx: &egui::Context, incoming: Vec<link_ipc::LayerView>) {
        let mut next = Vec::with_capacity(incoming.len());
        for view in incoming {
            let existing = self.layers.iter().find(|layer| layer.id == view.id);
            let mut thumbnail = existing.and_then(|layer| layer.thumbnail.clone());
            let mut thumbnail_b64 = existing.and_then(|layer| layer.thumbnail_b64.clone());
            if let Some(png) = view.thumbnail_png.clone() {
                if thumbnail_b64.as_deref() != Some(png.as_str()) {
                    if let Some(texture) = decode_thumbnail(ctx, view.id, &png) {
                        thumbnail = Some(texture);
                        thumbnail_b64 = Some(png);
                    }
                }
            }
            // 正在改名的图层不要被状态刷新覆盖输入框。
            let name = match &self.rename {
                Some((index, buffer)) if *index == view.id => buffer.clone(),
                _ => view.name.clone(),
            };
            next.push(LayerUi {
                id: view.id,
                name,
                visible: view.visible,
                thumbnail,
                thumbnail_b64,
            });
        }
        self.layers = next;
    }

    fn toast(&mut self, ctx: &egui::Context, message: String) {
        log::info!("{message}");
        self.error = Some((message, Instant::now()));
        ctx.request_repaint_after(Duration::from_secs(3));
    }
}

fn decode_thumbnail(
    ctx: &egui::Context,
    id: usize,
    base64_png: &str,
) -> Option<egui::TextureHandle> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_png)
        .ok()?;
    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    Some(ctx.load_texture(
        format!("layer-thumb-{id}"),
        color_image,
        egui::TextureOptions::LINEAR,
    ))
}

fn install_fonts(ctx: &egui::Context) {
    // 中文回退：优先用系统里能直接解析的 TTF（TTC 集合某些后端解析不了）。
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttf",
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simhei.ttf",
    ];
    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "cjk".to_string(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push("cjk".to_string());
        }
        ctx.set_fonts(fonts);
        log::info!("中文回退字体：{path}");
        return;
    }
    log::warn!("没找到中文回退字体，中文可能显示为方框");
}

fn install_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Light);
    ctx.all_styles_mut(|style| {
        style.visuals = egui::Visuals::light();
        style.visuals.panel_fill = Color32::TRANSPARENT;
        style.visuals.window_fill = CARD_BG;
        style.visuals.window_stroke = Stroke::new(1.0, BORDER);
        style.visuals.override_text_color = Some(TEXT);
        style.visuals.widgets.noninteractive.bg_stroke = Stroke::NONE;
        style.visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(0xE4, 0xE4, 0xEC);
        style.visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(0xE4, 0xE4, 0xEC);
        style.visuals.widgets.inactive.bg_stroke = Stroke::NONE;
        style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0xF0, 0xF0, 0xF6);
        style.visuals.widgets.hovered.bg_fill = ACCENT;
        style.visuals.widgets.hovered.bg_stroke = Stroke::NONE;
        style.visuals.widgets.active.weak_bg_fill = ACCENT_SOFT;
        style.visuals.widgets.active.bg_fill = ACCENT;
        style.visuals.widgets.active.bg_stroke = Stroke::NONE;
        style.visuals.selection.bg_fill = ACCENT;
        style.visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(8.0, 4.0);
        style.spacing.slider_width = 150.0;
    });
}

impl eframe::App for PanelApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump(&ctx);
        self.poll_pick(&ctx);
        self.handle_shortcuts(&ctx);
        self.keep_on_top(&ctx);

        let mut actions: Vec<Action> = Vec::new();

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(PANEL_BG)
                    .corner_radius(CornerRadius::same(RADIUS_PANEL))
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                self.titlebar(ui, &ctx, &mut actions);
                ui.add_space(8.0);
                self.passthrough_row(ui, &mut actions);
                separator(ui);
                self.tools_row(ui, &mut actions);
                separator(ui);
                self.colors_grid(ui, &mut actions);
                separator(ui);
                self.sliders(ui, &mut actions);
                separator(ui);
                self.list_height = (ui.available_height() - 132.0).max(72.0);
                self.layers_section(ui, &mut actions);
                ui.add_space(6.0);
                separator(ui);
                self.footer(ui, &mut actions);
            });

        self.export_window(&ctx, &mut actions);
        self.rename_window(&ctx, &mut actions);
        self.preview_window(&ctx);
        self.toast_banner(&ctx);

        for action in actions {
            self.dispatch(&ctx, action);
        }
    }
}

impl PanelApp {
    /// 画布是“全屏 + 置顶”的窗口，且比面板后创建，会压在面板上面吞掉点击。
    /// 画布就绪（以及每次导出后画布重新 show）之后，把面板重新提到最前。
    fn keep_on_top(&mut self, ctx: &egui::Context) {
        let Some(at) = self.raise_at else {
            return;
        };
        if Instant::now() >= at {
            if let Some(window) = self.x11_window {
                raise_window(window);
            }
            log::info!("把面板提到画布之上");
            self.raise_at = None;
        } else {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
    }

    fn titlebar(&self, ui: &mut egui::Ui, ctx: &egui::Context, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 26.0).max(40.0);
            let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::drag());
            if response.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            let painter = ui.painter();
            painter.text(
                rect.left_center() + Vec2::new(2.0, 0.0),
                Align2::LEFT_CENTER,
                "LInk",
                FontId::proportional(15.0),
                TEXT,
            );
            painter.circle_filled(
                Pos2::new(rect.left() + 46.0, rect.center().y),
                3.0,
                if self.ready { ACCENT } else { DIM },
            );
            painter.text(
                Pos2::new(rect.left() + 54.0, rect.center().y),
                Align2::LEFT_CENTER,
                if self.ready { "已连接" } else { "连接中" },
                FontId::proportional(11.0),
                DIM,
            );
            if icon_button(ui, Glyph::Close, Vec2::splat(24.0), false, "退出") {
                actions.push(Action::Quit);
            }
        });
    }

    fn passthrough_row(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("穿透模式").font(body()).color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = self.passthrough;
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::new(38.0, 20.0), Sense::click());
                let painter = ui.painter();
                let bg = if enabled {
                    ACCENT
                } else {
                    Color32::from_rgb(0xDD, 0xDD, 0xE6)
                };
                painter.rect_filled(rect, CornerRadius::same(10), bg);
                let knob_x = if enabled {
                    rect.right() - 10.0
                } else {
                    rect.left() + 10.0
                };
                painter.circle_filled(Pos2::new(knob_x, rect.center().y), 7.0, Color32::WHITE);
                if response.clicked() {
                    actions.push(Action::Passthrough(!enabled));
                }
            });
        });
        if self.passthrough {
            ui.label(
                RichText::new("鼠标与笔事件已交给下方应用，快捷键 F8 退出穿透")
                    .font(small())
                    .color(DIM),
            );
        }
    }

    fn tools_row(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        section_label(ui, "工具");
        ui.horizontal(|ui| {
            let tools = [
                (Glyph::Pen, ToolDto::Pen, "画笔 (P)"),
                (Glyph::Eraser, ToolDto::Eraser, "橡皮 (E)"),
                (Glyph::Line, ToolDto::Line, "直线 (L)"),
                (Glyph::Rect, ToolDto::Rect, "矩形 (R)"),
                (Glyph::Ellipse, ToolDto::Ellipse, "椭圆 (O)"),
            ];
            for (glyph, tool, hint) in tools {
                if icon_button(ui, glyph, Vec2::splat(TOOL_BUTTON), self.tool == tool, hint) {
                    actions.push(Action::SetTool(tool));
                }
            }
        });
    }

    fn colors_grid(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        section_label(ui, "颜色");
        const PALETTE: [[u8; 4]; 12] = [
            [28, 28, 34, 255],
            [220, 38, 38, 255],
            [234, 88, 12, 255],
            [202, 138, 4, 255],
            [22, 163, 74, 255],
            [13, 148, 136, 255],
            [37, 99, 235, 255],
            [81, 71, 229, 255],
            [147, 51, 234, 255],
            [219, 39, 119, 255],
            [120, 113, 108, 255],
            [255, 255, 255, 255],
        ];
        for (row, chunk) in PALETTE.chunks(6).enumerate() {
            ui.horizontal(|ui| {
                for (column, rgba) in chunk.iter().enumerate() {
                    let selected = self.color == *rgba;
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::splat(SWATCH), Sense::click());
                    let painter = ui.painter();
                    painter.circle_filled(
                        rect.center(),
                        SWATCH * 0.5,
                        Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]),
                    );
                    if selected {
                        painter.circle_stroke(
                            rect.center(),
                            SWATCH * 0.5 + 2.0,
                            Stroke::new(2.0, ACCENT),
                        );
                    } else if response.hovered() {
                        painter.circle_stroke(
                            rect.center(),
                            SWATCH * 0.5 + 1.0,
                            Stroke::new(1.0, BORDER),
                        );
                    }
                    if response.clicked() {
                        actions.push(Action::SetColor(*rgba));
                    }
                    let _ = (row, column);
                }
            });
        }
        ui.horizontal(|ui| {
            let mut color = Color32::from_rgba_unmultiplied(
                self.color[0],
                self.color[1],
                self.color[2],
                self.color[3],
            );
            if ui.color_edit_button_srgba(&mut color).changed() {
                actions.push(Action::SetColor([
                    color.r(),
                    color.g(),
                    color.b(),
                    color.a(),
                ]));
            }
            ui.label(RichText::new("自定义").font(small()).color(DIM));
        });
    }

    fn sliders(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        section_label(ui, "笔迹");
        ui.horizontal(|ui| {
            field_label(ui, "笔宽");
            let response = ui.add(
                egui::Slider::new(&mut self.width, 0.5..=20.0)
                    .fixed_decimals(1)
                    .suffix(" pt"),
            );
            if response.changed() {
                actions.push(Action::SetWidth(self.width));
            }
        });
        ui.horizontal(|ui| {
            field_label(ui, "压感曲线");
            let response = ui.add(egui::Slider::new(&mut self.gamma, 0.5..=3.0).fixed_decimals(2));
            if response.changed() {
                actions.push(Action::SetPressure);
            }
        });
        ui.horizontal(|ui| {
            field_label(ui, "最细比例");
            let response =
                ui.add(egui::Slider::new(&mut self.min_ratio, 0.05..=1.0).fixed_decimals(2));
            if response.changed() {
                actions.push(Action::SetPressure);
            }
        });
    }

    fn layers_section(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("图层").font(body()).color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Glyph::Down, Vec2::splat(26.0), false, "下移") {
                    actions.push(Action::MoveLayer(false));
                }
                if icon_button(ui, Glyph::Up, Vec2::splat(26.0), false, "上移") {
                    actions.push(Action::MoveLayer(true));
                }
                if icon_button(ui, Glyph::Minus, Vec2::splat(26.0), false, "删除图层") {
                    actions.push(Action::RemoveLayer);
                }
                if icon_button(ui, Glyph::Plus, Vec2::splat(26.0), false, "新建图层") {
                    actions.push(Action::AddLayer);
                }
            });
        });
        ui.add_space(2.0);
        egui::ScrollArea::vertical()
            .max_height(self.list_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // 顶层显示在列表上方
                let mut items: Vec<(usize, usize, String, bool, Option<egui::TextureHandle>)> =
                    Vec::new();
                for (index, layer) in self.layers.iter().enumerate() {
                    items.push((
                        index,
                        layer.id,
                        layer.name.clone(),
                        layer.visible,
                        layer.thumbnail.clone(),
                    ));
                }
                items.reverse();
                for (index, id, name, visible, thumbnail) in items {
                    self.layer_row(ui, index, id, &name, visible, thumbnail, actions);
                }
            });
    }

    #[allow(clippy::too_many_arguments)]
    fn layer_row(
        &self,
        ui: &mut egui::Ui,
        index: usize,
        id: usize,
        name: &str,
        visible: bool,
        thumbnail: Option<egui::TextureHandle>,
        actions: &mut Vec<Action>,
    ) {
        let selected = index == self.active;
        let height = 34.0;
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
        let painter = ui.painter();
        if selected {
            fill_rounded(painter, rect, RADIUS_ROW, ACCENT_SOFT);
        } else if response.hovered() {
            fill_rounded(
                painter,
                rect,
                RADIUS_ROW,
                Color32::from_rgb(0xF2, 0xF2, 0xF7),
            );
        }
        if response.clicked() {
            actions.push(Action::SelectLayer(index));
        }

        // 眼睛
        let eye_rect = Rect::from_min_size(
            rect.left_top() + Vec2::new(5.0, (height - 22.0) / 2.0),
            Vec2::splat(22.0),
        );
        let eye_response = ui.interact(eye_rect, ui.scope_id().with(("eye", id)), Sense::click());
        glyphs::paint(
            painter,
            eye_rect.shrink(2.0),
            if visible {
                Glyph::EyeOpen
            } else {
                Glyph::EyeClosed
            },
            if visible { TEXT } else { DIM },
        );
        if eye_response.clicked() {
            actions.push(Action::ToggleLayer(index, !visible));
        }

        // 缩略图
        let thumb_rect = Rect::from_min_size(
            eye_rect.right_top() + Vec2::new(2.0, 1.0),
            Vec2::new(42.0, 22.0),
        );
        let thumb_response = ui.interact(
            thumb_rect,
            ui.scope_id().with(("thumb", id)),
            Sense::click(),
        );
        if let Some(texture) = thumbnail {
            let size = texture.size_vec2();
            let scale = (thumb_rect.width() / size.x).min(thumb_rect.height() / size.y);
            let draw_size = size * scale;
            let target = Rect::from_center_size(thumb_rect.center(), draw_size);
            painter.image(
                texture.id(),
                target,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            painter.rect_stroke(
                target.expand(0.5),
                CornerRadius::same(2),
                Stroke::new(1.0, BORDER),
                egui::StrokeKind::Outside,
            );
        } else {
            fill_rounded(painter, thumb_rect, 3, Color32::from_rgb(0xEC, 0xEC, 0xF2));
        }
        if thumb_response.clicked() {
            actions.push(Action::Preview(id));
        }

        // 名字
        let name_rect = Rect::from_min_max(
            Pos2::new(thumb_rect.right() + 8.0, rect.top()),
            Pos2::new(rect.right() - 6.0, rect.bottom()),
        );
        painter.text(
            name_rect.left_center(),
            Align2::LEFT_CENTER,
            name,
            body(),
            if selected { ACCENT } else { TEXT },
        );
        let name_response =
            ui.interact(name_rect, ui.scope_id().with(("name", id)), Sense::click());
        if name_response.double_clicked() {
            actions.push(Action::BeginRename(index, name.to_string()));
        }
    }

    fn footer(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            if icon_button(
                ui,
                Glyph::Folder,
                Vec2::splat(30.0),
                false,
                "打开工程 (Ctrl+O)",
            ) {
                actions.push(Action::Open);
            }
            if icon_button(
                ui,
                Glyph::Save,
                Vec2::splat(30.0),
                false,
                "保存工程 (Ctrl+S)",
            ) {
                actions.push(Action::Save);
            }
            if icon_button(
                ui,
                Glyph::Export,
                Vec2::splat(30.0),
                false,
                "导出图片 (Ctrl+E)",
            ) {
                actions.push(Action::Export);
            }
            let remaining = ui.available_width() - 30.0;
            if remaining > 0.0 {
                ui.add_space(remaining);
            }
            if icon_button(ui, Glyph::Trash, Vec2::splat(32.0), false, "清空当前图层") {
                actions.push(Action::ClearLayer);
            }
        });
        ui.add_space(2.0);
        let status = match &self.device {
            Some((name, tool, pressure, has_pressure)) => {
                if *has_pressure {
                    format!("{name} · {tool} · 压感 {:.0}%", pressure * 100.0)
                } else {
                    format!("{name} · {tool} · 无压感")
                }
            }
            None => "等待数位笔…".to_string(),
        };
        ui.label(RichText::new(status).font(small()).color(DIM));

        if let Some(error) = &self.startup_error {
            ui.label(RichText::new(error).font(small()).color(DANGER));
        }
    }

    fn export_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.show_export {
            return;
        }
        let mut open = self.show_export;
        egui::Window::new("导出图片")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(RichText::new("格式").font(small()).color(DIM));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.export_format, ImageFormatDto::Png, "PNG");
                    ui.selectable_value(&mut self.export_format, ImageFormatDto::Jpeg, "JPEG");
                });
                ui.add_space(6.0);
                ui.label(RichText::new("背景").font(small()).color(DIM));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.export_screen, false, "空白");
                    ui.selectable_value(&mut self.export_screen, true, "当前屏幕");
                });
                ui.add_space(10.0);
                if ui.button("选择位置并导出").clicked() {
                    let background = if self.export_screen {
                        BackgroundDto::Screen
                    } else {
                        BackgroundDto::Blank
                    };
                    actions.push(Action::ExportTo {
                        format: self.export_format,
                        background,
                    });
                    self.show_export = false;
                }
            });
        self.show_export = open;
    }

    fn rename_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some((index, mut buffer)) = self.rename.clone() else {
            return;
        };
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;
        egui::Window::new("重命名图层")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let response = ui.add(egui::TextEdit::singleline(&mut buffer).desired_width(220.0));
                if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("确定").clicked() {
                        submit = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        if submit && !buffer.trim().is_empty() {
            actions.push(Action::RenameLayer(index, buffer.trim().to_string()));
            self.rename = None;
        } else if cancel || !open {
            self.rename = None;
        } else {
            self.rename = Some((index, buffer));
        }
    }

    fn preview_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.preview else {
            return;
        };
        let Some(layer) = self.layers.iter().find(|layer| layer.id == id) else {
            self.preview = None;
            return;
        };
        let name = layer.name.clone();
        let texture = layer.thumbnail.clone();
        let mut open = true;
        egui::Window::new(format!("图层预览 · {name}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                if let Some(texture) = texture {
                    let size = texture.size_vec2();
                    let scale = (560.0 / size.x).min(320.0 / size.y).max(0.2);
                    ui.image((texture.id(), size * scale));
                } else {
                    ui.label(RichText::new("这一层还没有内容").font(body()).color(DIM));
                }
            });
        if !open {
            self.preview = None;
        }
    }

    fn toast_banner(&mut self, ctx: &egui::Context) {
        let Some((message, at)) = self.error.clone() else {
            return;
        };
        if at.elapsed() > Duration::from_secs(3) {
            self.error = None;
            return;
        }
        egui::Area::new(egui::Id::unique("toast"))
            .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -18.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgb(0x24, 0x24, 0x2C))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.label(RichText::new(message).font(body()).color(Color32::WHITE));
                    });
            });
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (p, e, l, r, o, f8, ctrl_s, ctrl_o, ctrl_e) = ctx.input(|input| {
            (
                input.key_pressed(egui::Key::P),
                input.key_pressed(egui::Key::E),
                input.key_pressed(egui::Key::L),
                input.key_pressed(egui::Key::R),
                input.key_pressed(egui::Key::O),
                input.key_pressed(egui::Key::F8),
                input.modifiers.command && input.key_pressed(egui::Key::S),
                input.modifiers.command && input.key_pressed(egui::Key::O),
                input.modifiers.command && input.key_pressed(egui::Key::E),
            )
        });
        if ctrl_s {
            self.dispatch(ctx, Action::Save);
        }
        if ctrl_o {
            self.dispatch(ctx, Action::Open);
        }
        if ctrl_e {
            self.show_export = true;
        }
        if f8 {
            self.dispatch(ctx, Action::Passthrough(!self.passthrough));
        }
        if p {
            self.dispatch(ctx, Action::SetTool(ToolDto::Pen));
        }
        if e {
            self.dispatch(ctx, Action::SetTool(ToolDto::Eraser));
        }
        if l {
            self.dispatch(ctx, Action::SetTool(ToolDto::Line));
        }
        if r {
            self.dispatch(ctx, Action::SetTool(ToolDto::Rect));
        }
        if o {
            self.dispatch(ctx, Action::SetTool(ToolDto::Ellipse));
        }
    }
}

/// 取本窗口的 X11 id（winit 在 Linux 上用 Xlib/Xcb 窗口）。
#[cfg(all(unix, not(target_os = "macos")))]
fn x11_window_id(cc: &eframe::CreationContext<'_>) -> Option<u32> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(handle) => Some(handle.window as u32),
        RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
        _ => None,
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn x11_window_id(_cc: &eframe::CreationContext<'_>) -> Option<u32> {
    None
}

/// 直接把面板窗口抬到最前。
///
/// `ViewportCommand::Focus` 只改焦点，窗口管理器不会因此重排两个“置顶”窗口，
/// 而画布是后创建的全屏置顶窗口，会盖住面板、吞掉点击。
#[cfg(all(unix, not(target_os = "macos")))]
fn raise_window(window: u32) {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt as _, StackMode};
    if let Ok((connection, screen)) = x11rb::connect(None) {
        let aux = ConfigureWindowAux::new().stack_mode(StackMode::ABOVE);
        if let Err(error) = connection.configure_window(window, &aux) {
            log::warn!("抬升面板窗口失败: {error}");
        }
        let _ = connection.flush();
        let _ = screen;
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn raise_window(_window: u32) {}

/// 固定宽度的字段名，保证三条滑杆左端对齐。
fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(
        Vec2::new(62.0, 18.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(RichText::new(text).font(body()).color(DIM));
        },
    );
}

fn separator(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, BORDER);
    ui.add_space(6.0);
}

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([352.0, 792.0])
            .with_min_inner_size([352.0, 560.0])
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_title("LInk"),
        ..Default::default()
    };

    eframe::run_native(
        "LInk",
        options,
        Box::new(|cc| Ok(Box::new(PanelApp::new(cc)))),
    )
}
