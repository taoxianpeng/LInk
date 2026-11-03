//! LInk 面板（egui）：纯 Rust、无 webview 的控制面板。
//!
//! 画布是一个独立进程（`link-ink-canvas`），本进程只负责界面和命令下发，
//! 两者用 `link-ipc` 的 JSON lines 协议通信。

mod dialog;
mod glyphs;
mod ipc;
mod settings;
mod theme;

use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2,
};
use link_ipc::{BackgroundDto, Command, Event, ImageFormatDto, ToolDto};

use crate::glyphs::Glyph;
use crate::settings::Settings;
use crate::theme::*;

const TOOL_BUTTON: f32 = 34.0;
const SWATCH: f32 = 16.0;
const ACTION_BUTTON: f32 = 24.0;
const LAYER_LIST_MAX_HEIGHT: f32 = 150.0;
const PANEL_WIDTH: f32 = 300.0;
const PANEL_MIN_HEIGHT: f32 = 320.0;
const PANEL_MAX_HEIGHT: f32 = 640.0;

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
    SetWidthPreset(usize),
    ApplyWidth,
    SetPressure,
    OpenSettings,
    AddLayer,
    RemoveLayer,
    MoveLayer(bool),
    SelectLayer(usize),
    RenameLayer(usize, String),
    ToggleLayer(usize, bool),
    ClearLayer,
    RecolorSelection([u8; 4]),
    ScaleSelection(f64),
    MoveSelectionToLayer(usize),
    ClearSelection,
    Undo,
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
    selection_count: usize,
    selection_color: [u8; 4],
    settings: Settings,
    show_settings: bool,
    settings_dirty: bool,
    settings_save_at: Option<Instant>,

    device: Option<(String, String, f64, bool)>,
    error: Option<(String, Instant)>,

    rename: Option<(usize, String)>,
    preview: Option<usize>,
    show_export: bool,
    export_format: ImageFormatDto,
    export_screen: bool,

    ready: bool,
    start: Instant,
    pending: Option<PendingPick>,
    /// 需要把面板提到画布之上（画布是全屏置顶窗口，会盖住面板）。
    raise_at: Option<Instant>,
    /// 本面板窗口的原生句柄（Linux 是 X11 window id，Windows 是 HWND）。
    native_window: Option<isize>,
}

impl PanelApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        install_style(&cc.egui_ctx);

        let (client, startup_error) = match ipc::CanvasClient::spawn(cc.egui_ctx.clone()) {
            Ok(client) => (Some(client), None),
            Err(error) => (None, Some(error)),
        };
        let settings = Settings::load();

        Self {
            client,
            startup_error,
            layers: Vec::new(),
            active: 0,
            passthrough: false,
            tool: ToolDto::Pen,
            color: [0x1C, 0x1C, 0x22, 0xFF],
            selection_count: 0,
            selection_color: [0xDC, 0x26, 0x26, 0xFF],
            settings,
            show_settings: false,
            settings_dirty: false,
            settings_save_at: None,
            device: None,
            error: None,
            rename: None,
            preview: None,
            show_export: false,
            export_format: ImageFormatDto::Png,
            export_screen: false,
            ready: false,
            start: Instant::now(),
            pending: None,
            raise_at: None,
            native_window: native_window_id(cc),
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
            Action::SetWidthPreset(index) => {
                self.settings.set_preset(index);
                self.send(Command::SetWidth {
                    pt: self.settings.width(),
                });
                self.mark_settings_dirty(ctx);
            }
            Action::ApplyWidth => {
                self.send(Command::SetWidth {
                    pt: self.settings.width(),
                });
                self.mark_settings_dirty(ctx);
            }
            Action::SetPressure => {
                self.send(Command::SetPressure {
                    gamma: self.settings.gamma,
                    min_ratio: self.settings.min_ratio,
                });
                self.mark_settings_dirty(ctx);
            }
            Action::OpenSettings => {
                self.show_settings = true;
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
            Action::RecolorSelection(rgba) => {
                self.send(Command::RecolorSelection { rgba });
            }
            Action::ScaleSelection(factor) => {
                self.send(Command::ScaleSelection { factor });
            }
            Action::MoveSelectionToLayer(index) => {
                self.send(Command::MoveSelectionToLayer { index });
            }
            Action::ClearSelection => self.send(Command::ClearSelection),
            Action::Undo => self.send(Command::Undo),
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
                self.save_settings_now();
                // 先显式关掉画布（kill + wait），再走 eframe 的正常关闭流程。
                // `std::process::exit` 会跳过所有 Drop，画布可能变成孤儿进程。
                self.client.take();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
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
                    if proto != link_ipc::PROTOCOL_VERSION {
                        // 典型场景：cargo run 只重建了面板，画布还是旧的。
                        let message = format!(
                            "画布版本不匹配（面板 {}，画布 {}）：请执行 `cargo build --workspace` 重新构建两个可执行文件",
                            link_ipc::PROTOCOL_VERSION,
                            proto
                        );
                        log::error!("{message}");
                        self.toast(ctx, message);
                        self.client = None;
                        self.startup_error = Some("画布与面板版本不一致，已停止连接".to_string());
                        return;
                    }
                    self.ready = true;
                    log::info!(
                        "画布就绪：协议 {proto}，显示器 {}x{}",
                        monitor.width,
                        monitor.height
                    );
                    self.send(Command::SetWidth {
                        pt: self.settings.width(),
                    });
                    self.send(Command::SetPressure {
                        gamma: self.settings.gamma,
                        min_ratio: self.settings.min_ratio,
                    });
                    let elapsed = self.start.elapsed();
                    log::info!("面板冷启动 -> 画布就绪: {:?}", elapsed);
                    self.raise_at = Some(Instant::now() + Duration::from_millis(250));
                }
                Event::State {
                    layers,
                    active,
                    passthrough,
                    selection,
                } => {
                    self.passthrough = passthrough;
                    self.active = active;
                    self.selection_count = selection;
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
                    // 握手阶段的错误几乎都是"新面板配旧画布"：旧二进制只会报它自己的
                    // 版本号，所以由面板补上"该怎么办"，否则用户看到报错也不知道如何修。
                    if !self.ready {
                        let hint = format!(
                            "{message}\n请执行 `cargo build --workspace` 重新构建两个可执行文件"
                        );
                        log::error!("{hint}");
                        self.toast(ctx, hint);
                        self.client = None;
                        self.startup_error = Some("画布未能建立连接".to_string());
                        return;
                    }
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

    fn mark_settings_dirty(&mut self, ctx: &egui::Context) {
        self.settings_dirty = true;
        self.settings_save_at = Some(Instant::now() + Duration::from_millis(400));
        ctx.request_repaint_after(Duration::from_millis(450));
    }

    fn flush_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_dirty {
            return;
        }
        if let Some(at) = self.settings_save_at {
            if Instant::now() < at {
                ctx.request_repaint_after(Duration::from_millis(80));
                return;
            }
        }
        self.save_settings_now();
    }

    fn save_settings_now(&mut self) {
        match self.settings.save() {
            Ok(()) => log::debug!("设置已保存"),
            Err(error) => log::warn!("保存设置失败: {error}"),
        }
        self.settings_dirty = false;
        self.settings_save_at = None;
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
    // 中文回退：egui 0.37 用 skrifa 解析，TTC（含 msyh.ttc）可以正常加载。
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        let font_dir = std::env::var_os("WINDIR")
            .map(std::path::PathBuf::from)
            .map(|dir| dir.join("Fonts"))
            .unwrap_or_else(|| std::path::PathBuf::from("C:/Windows/Fonts"));
        for name in ["msyh.ttc", "msyhl.ttc", "simhei.ttf", "simsun.ttc"] {
            candidates.push(font_dir.join(name));
        }
    }
    #[cfg(target_os = "linux")]
    {
        for path in [
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttf",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        ] {
            candidates.push(std::path::PathBuf::from(path));
        }
    }

    for path in candidates {
        let Ok(bytes) = std::fs::read(&path) else {
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
        log::info!("中文回退字体：{}", path.display());
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
        style.spacing.item_spacing = Vec2::new(6.0, 5.0);
        style.spacing.button_padding = Vec2::new(6.0, 3.0);
        style.spacing.slider_width = 140.0;
    });
}

impl eframe::App for PanelApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Linux 靠窗口透明 + 圆角面板；Windows 的 WGL 透明度不稳定，
        // 直接用不透明底色，避免圆角处出现黑边。
        #[cfg(target_os = "windows")]
        {
            let bg = PANEL_BG;
            [
                bg.r() as f32 / 255.0,
                bg.g() as f32 / 255.0,
                bg.b() as f32 / 255.0,
                1.0,
            ]
        }
        #[cfg(not(target_os = "windows"))]
        {
            [0.0, 0.0, 0.0, 0.0]
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump(&ctx);
        self.poll_pick(&ctx);
        self.handle_shortcuts(&ctx);
        self.keep_on_top(&ctx);

        let mut actions: Vec<Action> = Vec::new();
        let mut content_bottom = 0.0;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(PANEL_BG)
                    .corner_radius(CornerRadius::same(RADIUS_PANEL))
                    .inner_margin(egui::Margin::same(10)),
            )
            .show(ui, |ui| {
                self.titlebar(ui, &ctx, &mut actions);
                self.passthrough_row(ui, &mut actions);
                separator(ui);
                self.tools_row(ui, &mut actions);
                self.colors_row(ui, &mut actions);
                self.width_row(ui, &mut actions);
                self.selection_row(ui, &mut actions);
                separator(ui);
                self.layers_section(ui, &mut actions);
                content_bottom = self.footer(ui, &mut actions);
            });

        self.fit_height(&ctx, content_bottom);
        self.export_window(&ctx, &mut actions);
        self.settings_window(&ctx, &mut actions);
        self.rename_window(&ctx, &mut actions);
        self.preview_window(&ctx);
        self.toast_banner(&ctx);
        self.flush_settings(&ctx);

        for action in actions {
            self.dispatch(&ctx, action);
        }
    }
}

impl PanelApp {
    /// 面板高度跟随内容（`content_bottom` 是逻辑点），避免图层少时留下大块空白。
    ///
    /// `ViewportCommand::InnerSize` 的单位是逻辑点，winit 会按当前 DPI 换算成物理像素；
    /// 这里比较的也是 `content_rect` 的逻辑尺寸，两边单位一致。
    fn fit_height(&self, ctx: &egui::Context, content_bottom: f32) {
        if !content_bottom.is_finite() || content_bottom <= 0.0 {
            return;
        }
        let screen = ctx.content_rect();
        let desired = (content_bottom - screen.top() + 10.0)
            .ceil()
            .clamp(PANEL_MIN_HEIGHT, PANEL_MAX_HEIGHT);
        // 留 2pt 容差：非整数 DPI（1.25/1.5/1.75）下取整会造成 0.5px 抖动死循环。
        if (desired - screen.height()).abs() > 2.0 {
            log::debug!("调整面板高度：{:.1} -> {desired:.1}", screen.height());
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(
                screen.width().max(PANEL_WIDTH),
                desired,
            )));
        }
    }

    /// 画布是“全屏 + 置顶”的窗口，且比面板后创建，会压在面板上面吞掉点击。
    /// 画布就绪（以及每次导出后画布重新 show）之后，把面板重新提到最前。
    fn keep_on_top(&mut self, ctx: &egui::Context) {
        let Some(at) = self.raise_at else {
            return;
        };
        if Instant::now() >= at {
            if let Some(window) = self.native_window {
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
            let width = (ui.available_width() - 50.0).max(40.0);
            let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::drag());
            if response.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            let painter = ui.painter();
            painter.text(
                rect.left_center() + Vec2::new(2.0, 0.0),
                Align2::LEFT_CENTER,
                "LInk",
                FontId::proportional(14.0),
                TEXT,
            );
            painter.circle_filled(
                Pos2::new(rect.left() + 40.0, rect.center().y),
                3.0,
                if self.ready { ACCENT } else { DIM },
            );
            response.on_hover_text(if self.ready {
                "画布已连接"
            } else {
                "画布连接中"
            });
            if icon_button(ui, Glyph::Gear, Vec2::splat(22.0), false, "设置") {
                actions.push(Action::OpenSettings);
            }
            if icon_button(ui, Glyph::Close, Vec2::splat(22.0), false, "退出") {
                actions.push(Action::Quit);
            }
        });
    }

    fn passthrough_row(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("穿透").font(body()).color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = self.passthrough;
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::new(34.0, 18.0), Sense::click());
                let painter = ui.painter();
                let bg = if enabled {
                    ACCENT
                } else {
                    Color32::from_rgb(0xDD, 0xDD, 0xE6)
                };
                painter.rect_filled(rect, CornerRadius::same(9), bg);
                let knob_x = if enabled {
                    rect.right() - 9.0
                } else {
                    rect.left() + 9.0
                };
                painter.circle_filled(Pos2::new(knob_x, rect.center().y), 6.0, Color32::WHITE);
                if response
                    .on_hover_text("鼠标与笔事件穿透到下层 (F8)")
                    .clicked()
                {
                    actions.push(Action::Passthrough(!enabled));
                }
            });
        });
    }

    fn tools_row(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            let tools = [
                (Glyph::Pen, ToolDto::Pen, "画笔 (P)"),
                (Glyph::Eraser, ToolDto::Eraser, "橡皮 (E)"),
                (Glyph::Line, ToolDto::Line, "直线 (L)"),
                (Glyph::Rect, ToolDto::Rect, "矩形 (R)"),
                (Glyph::Ellipse, ToolDto::Ellipse, "椭圆 (O)"),
                (Glyph::SelectRect, ToolDto::SelectRect, "矩形选择 (S)"),
                (Glyph::SelectLasso, ToolDto::SelectLasso, "自由选择 (F)"),
            ];
            for (glyph, tool, hint) in tools {
                if icon_button(ui, glyph, Vec2::splat(TOOL_BUTTON), self.tool == tool, hint) {
                    actions.push(Action::SetTool(tool));
                }
            }
        });
    }

    fn colors_row(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
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
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for rgba in PALETTE {
                let selected = self.color == rgba;
                let (rect, response) = ui.allocate_exact_size(Vec2::splat(SWATCH), Sense::click());
                let painter = ui.painter();
                painter.circle_filled(
                    rect.center(),
                    SWATCH * 0.5,
                    Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]),
                );
                let luminance =
                    0.299 * rgba[0] as f32 + 0.587 * rgba[1] as f32 + 0.114 * rgba[2] as f32;
                if luminance > 220.0 && !selected {
                    painter.circle_stroke(rect.center(), SWATCH * 0.5, Stroke::new(1.0, BORDER));
                }
                if selected {
                    painter.circle_stroke(
                        rect.center(),
                        SWATCH * 0.5 + 1.5,
                        Stroke::new(2.0, ACCENT),
                    );
                }
                if response.clicked() {
                    actions.push(Action::SetColor(rgba));
                }
            }
            let mut color = Color32::from_rgba_unmultiplied(
                self.color[0],
                self.color[1],
                self.color[2],
                self.color[3],
            );
            if ui
                .color_edit_button_srgba(&mut color)
                .on_hover_text("自定义颜色")
                .changed()
            {
                actions.push(Action::SetColor([
                    color.r(),
                    color.g(),
                    color.b(),
                    color.a(),
                ]));
            }
        });
    }

    fn width_row(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("笔宽").font(small()).color(DIM));
            ui.spacing_mut().item_spacing.x = 4.0;
            for (index, label) in settings::PRESET_LABELS.iter().enumerate() {
                let selected = self.settings.width_preset == index;
                let hint = format!("{label} · {:.1} pt", self.settings.width_presets[index]);
                if ui
                    .selectable_label(selected, RichText::new(*label).font(small()))
                    .on_hover_text(hint)
                    .clicked()
                {
                    actions.push(Action::SetWidthPreset(index));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{:.1} pt", self.settings.width()))
                        .font(small())
                        .color(DIM),
                );
            });
        });
    }

    fn selection_row(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        if self.selection_count == 0 {
            return;
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("已选 {}", self.selection_count))
                    .font(small())
                    .color(ACCENT),
            );
            ui.spacing_mut().item_spacing.x = 4.0;
            if icon_button(ui, Glyph::Minus, Vec2::splat(ACTION_BUTTON), false, "缩小") {
                actions.push(Action::ScaleSelection(1.0 / 1.1));
            }
            if icon_button(ui, Glyph::Plus, Vec2::splat(ACTION_BUTTON), false, "放大") {
                actions.push(Action::ScaleSelection(1.1));
            }
            let mut color = Color32::from_rgba_unmultiplied(
                self.selection_color[0],
                self.selection_color[1],
                self.selection_color[2],
                self.selection_color[3],
            );
            if ui
                .color_edit_button_srgba(&mut color)
                .on_hover_text("选区颜色")
                .changed()
            {
                self.selection_color = [color.r(), color.g(), color.b(), color.a()];
                actions.push(Action::RecolorSelection(self.selection_color));
            }
            let mut target = None;
            egui::ComboBox::from_id_salt("selection_layer")
                .selected_text(RichText::new("→ 图层").font(small()))
                .width(72.0)
                .show_ui(ui, |ui| {
                    for (index, layer) in self.layers.iter().enumerate() {
                        if ui.selectable_label(false, &layer.name).clicked() {
                            target = Some(index);
                        }
                    }
                });
            if let Some(index) = target {
                actions.push(Action::MoveSelectionToLayer(index));
            }
            if icon_button(
                ui,
                Glyph::Close,
                Vec2::splat(ACTION_BUTTON),
                false,
                "清除选择",
            ) {
                actions.push(Action::ClearSelection);
            }
        });
    }

    fn layers_section(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("图层 · {}", self.layers.len()))
                    .font(small())
                    .color(DIM),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if icon_button(ui, Glyph::Down, Vec2::splat(ACTION_BUTTON), false, "下移") {
                    actions.push(Action::MoveLayer(false));
                }
                if icon_button(ui, Glyph::Up, Vec2::splat(ACTION_BUTTON), false, "上移") {
                    actions.push(Action::MoveLayer(true));
                }
                if icon_button(
                    ui,
                    Glyph::Minus,
                    Vec2::splat(ACTION_BUTTON),
                    false,
                    "删除图层",
                ) {
                    actions.push(Action::RemoveLayer);
                }
                if icon_button(
                    ui,
                    Glyph::Plus,
                    Vec2::splat(ACTION_BUTTON),
                    false,
                    "新建图层",
                ) {
                    actions.push(Action::AddLayer);
                }
            });
        });
        ui.add_space(1.0);
        egui::ScrollArea::vertical()
            .max_height(LAYER_LIST_MAX_HEIGHT)
            .auto_shrink([false, true])
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
        let height = 28.0;
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
            rect.left_top() + Vec2::new(4.0, (height - 18.0) / 2.0),
            Vec2::splat(18.0),
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
            eye_rect.right_top() + Vec2::new(2.0, 0.0),
            Vec2::new(38.0, 18.0),
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
            Pos2::new(thumb_rect.right() + 6.0, rect.top()),
            Pos2::new(rect.right() - 4.0, rect.bottom()),
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
        // 名字区域盖住了整行的绝大部分，单击必须能选中图层；
        // 双击 = 选中并进入改名（与常见绘图软件一致）。
        if name_response.clicked() {
            actions.push(Action::SelectLayer(index));
        }
        if name_response.double_clicked() {
            actions.push(Action::BeginRename(index, name.to_string()));
        }
    }

    /// 返回状态行底边，供面板按内容调整高度。
    fn footer(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) -> f32 {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if icon_button(
                ui,
                Glyph::Folder,
                Vec2::splat(ACTION_BUTTON),
                false,
                "打开工程 (Ctrl+O)",
            ) {
                actions.push(Action::Open);
            }
            if icon_button(
                ui,
                Glyph::Save,
                Vec2::splat(ACTION_BUTTON),
                false,
                "保存工程 (Ctrl+S)",
            ) {
                actions.push(Action::Save);
            }
            if icon_button(
                ui,
                Glyph::Export,
                Vec2::splat(ACTION_BUTTON),
                false,
                "导出图片 (Ctrl+E)",
            ) {
                actions.push(Action::Export);
            }
            let remaining = ui.available_width() - ACTION_BUTTON;
            if remaining > 0.0 {
                ui.add_space(remaining);
            }
            if icon_button(
                ui,
                Glyph::Trash,
                Vec2::splat(ACTION_BUTTON),
                false,
                "清空当前图层",
            ) {
                actions.push(Action::ClearLayer);
            }
        });
        let status = match &self.device {
            Some((name, tool, pressure, has_pressure)) => {
                if *has_pressure {
                    format!("{name} · {tool} · {:.0}%", pressure * 100.0)
                } else {
                    format!("{name} · {tool} · 无压感")
                }
            }
            None => "等待笔输入".to_string(),
        };
        let response =
            ui.add(egui::Label::new(RichText::new(&status).font(small()).color(DIM)).truncate());
        let mut bottom = response.on_hover_text(&status).rect.bottom();

        if let Some(error) = &self.startup_error {
            bottom = ui
                .label(RichText::new(error).font(small()).color(DANGER))
                .rect
                .bottom();
        }
        bottom
    }

    fn settings_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.show_settings {
            return;
        }
        let mut open = self.show_settings;
        let mut width_changed = false;
        let mut pressure_changed = false;

        egui::Window::new("设置")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(250.0);
                ui.label(RichText::new("笔宽档位").font(small()).color(DIM));
                for (index, label) in settings::PRESET_LABELS.iter().enumerate() {
                    ui.horizontal(|ui| {
                        field_label(ui, label);
                        let mut value = self.settings.width_presets[index];
                        let response = ui.add(
                            egui::Slider::new(&mut value, 0.1..=20.0)
                                .fixed_decimals(1)
                                .suffix(" pt"),
                        );
                        if response.changed() {
                            self.settings.width_presets[index] = value;
                            width_changed = true;
                        }
                    });
                }
                ui.add_space(4.0);
                ui.label(RichText::new("压感").font(small()).color(DIM));
                ui.horizontal(|ui| {
                    field_label(ui, "曲线");
                    let response = ui.add(
                        egui::Slider::new(&mut self.settings.gamma, 0.5..=3.0).fixed_decimals(2),
                    );
                    if response.changed() {
                        pressure_changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    field_label(ui, "最细");
                    let response = ui.add(
                        egui::Slider::new(&mut self.settings.min_ratio, 0.05..=1.0)
                            .fixed_decimals(2),
                    );
                    if response.changed() {
                        pressure_changed = true;
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("恢复默认").clicked() {
                        self.settings = Settings::default();
                        width_changed = true;
                        pressure_changed = true;
                    }
                    if let Some(path) = settings::path() {
                        ui.label(RichText::new("设置文件").font(small()).color(DIM))
                            .on_hover_text(path.display().to_string());
                    }
                });
            });

        if width_changed {
            actions.push(Action::ApplyWidth);
        }
        if pressure_changed {
            actions.push(Action::SetPressure);
        }
        if !open {
            self.save_settings_now();
        }
        self.show_settings = open;
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
        // 有文本框在接收输入时不抢快捷键：否则在图层改名里输入 "p" 会切成画笔，
        // Ctrl+Z 会撤掉笔迹而不是编辑文字。
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (p, e, l, r, o, s, f, escape, f8, ctrl_s, ctrl_o, ctrl_e, ctrl_z) =
            ctx.input(|input| {
                let command = input.modifiers.command;
                (
                    input.key_pressed(egui::Key::P),
                    input.key_pressed(egui::Key::E),
                    input.key_pressed(egui::Key::L),
                    input.key_pressed(egui::Key::R),
                    input.key_pressed(egui::Key::O),
                    !command && input.key_pressed(egui::Key::S),
                    !command && input.key_pressed(egui::Key::F),
                    input.key_pressed(egui::Key::Escape),
                    input.key_pressed(egui::Key::F8),
                    command && input.key_pressed(egui::Key::S),
                    command && input.key_pressed(egui::Key::O),
                    command && input.key_pressed(egui::Key::E),
                    command && input.key_pressed(egui::Key::Z),
                )
            });
        if ctrl_z {
            self.dispatch(ctx, Action::Undo);
        }
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
        if s {
            self.dispatch(ctx, Action::SetTool(ToolDto::SelectRect));
        }
        if f {
            self.dispatch(ctx, Action::SetTool(ToolDto::SelectLasso));
        }
        if escape && self.selection_count > 0 {
            self.dispatch(ctx, Action::ClearSelection);
        }
    }
}

/// 取本窗口的原生句柄。
#[cfg(all(unix, not(target_os = "macos")))]
fn native_window_id(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(handle) => Some(handle.window as isize),
        RawWindowHandle::Xcb(handle) => Some(handle.window.get() as isize),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn native_window_id(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as isize),
        _ => None,
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
fn native_window_id(_cc: &eframe::CreationContext<'_>) -> Option<isize> {
    None
}

/// 直接把面板窗口抬到最前。
///
/// `ViewportCommand::Focus` 只改焦点，窗口管理器不会因此重排两个“置顶”窗口，
/// 而画布是后创建的全屏置顶窗口，会盖住面板、吞掉点击。
#[cfg(all(unix, not(target_os = "macos")))]
fn raise_window(window: isize) {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt as _, StackMode};
    if let Ok((connection, screen)) = x11rb::connect(None) {
        let aux = ConfigureWindowAux::new().stack_mode(StackMode::ABOVE);
        if let Err(error) = connection.configure_window(window as u32, &aux) {
            log::warn!("抬升面板窗口失败: {error}");
        }
        let _ = connection.flush();
        let _ = screen;
    }
}

/// Windows：两个同为 topmost 的窗口不会自动重排，需要显式再置顶一次。
#[cfg(target_os = "windows")]
fn raise_window(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };
    let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
    let ok = unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    };
    if ok == 0 {
        log::warn!("抬升面板窗口失败：{}", std::io::Error::last_os_error());
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
fn raise_window(_window: isize) {}

/// 固定宽度的字段名，保证滑杆左端对齐。
fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(
        Vec2::new(34.0, 16.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(RichText::new(text).font(small()).color(DIM));
        },
    );
}

fn separator(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, BORDER);
    ui.add_space(6.0);
}

#[cfg(target_os = "windows")]
fn enable_dpi_awareness() {
    // 必须在创建 winit 事件循环/窗口之前调用。winit 自己也会设，但如果进程清单里
    // 没有 DPI 感知声明，它可能拿到 scale_factor = 1.0，导致窗口物理尺寸只有
    // 逻辑尺寸那么大（内容被裁掉）。
    const PER_MONITOR_AWARE_V2: *mut std::ffi::c_void = (-4isize) as *mut std::ffi::c_void;
    let ok = unsafe {
        windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)
    };
    if ok == 0 {
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetProcessDPIAware();
        }
    }
}

/// wgpu 后端的候选集合。
///
/// Windows 上**必须**排除 Vulkan：Intel HD 530（以及同代驱动）在
/// `adapter.request_device()` 里会直接踩空指针 —— 返回不了 `Err`，
/// 整个进程 `STATUS_ACCESS_VIOLATION`，任何运行时回退都兜不住。
/// 主用 DX12，GL 兜底（老机器上 DX12 只剩 WARP 软件光栅时，
/// wgpu 按设备类型会优先挑 GL 而不是 CPU 适配器）。
/// `WGPU_BACKEND` 环境变量仍可覆盖。
#[cfg(target_os = "windows")]
fn wgpu_configuration() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::wgpu;

    let mut config = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        setup.instance_descriptor.backends =
            wgpu::Backends::from_env().unwrap_or(wgpu::Backends::DX12 | wgpu::Backends::GL);
    }
    config
}

#[cfg(not(target_os = "windows"))]
fn wgpu_configuration() -> eframe::egui_wgpu::WgpuConfiguration {
    eframe::egui_wgpu::WgpuConfiguration::default()
}

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    #[cfg(target_os = "windows")]
    enable_dpi_awareness();

    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([PANEL_WIDTH, 340.0])
        .with_min_inner_size([PANEL_WIDTH, PANEL_MIN_HEIGHT])
        .with_max_inner_size([PANEL_WIDTH, PANEL_MAX_HEIGHT])
        .with_resizable(true)
        .with_decorations(false)
        .with_always_on_top()
        .with_title("LInk");
    // Windows 的 WGL 透明合成不可靠（见 clear_color 注释），只在不透明模式下运行。
    #[cfg(not(target_os = "windows"))]
    let viewport = viewport.with_transparent(true);
    let options = eframe::NativeOptions {
        viewport,
        wgpu_options: wgpu_configuration(),
        ..Default::default()
    };

    eframe::run_native(
        "LInk",
        options,
        Box::new(|cc| Ok(Box::new(PanelApp::new(cc)))),
    )
}
