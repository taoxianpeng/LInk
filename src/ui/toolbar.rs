use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::app::{App, Tool};
use crate::export::{self, ExportBackground, ImageFormat};
use crate::model::Rgba;
use crate::platform::{CaptureImage, MonitorInfo, PlatformError};
use crate::render;
use crate::xopp;

/// 需要重建图层行的变化：图层数量、顺序、名字、可见性。
///
/// 注意这里**不含**每层笔数：落笔/擦除只改变内容，重建控件会让面板闪烁。
fn structure_key(app: &App) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    app.document.layers.len().hash(&mut hasher);
    for layer in &app.document.layers {
        layer.name.hash(&mut hasher);
        layer.visible.hash(&mut hasher);
    }
    hasher.finish()
}

/// 只需要重画缩略图的变化：每层笔数。
fn content_key(app: &App) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for layer in &app.document.layers {
        layer.strokes.len().hash(&mut hasher);
    }
    hasher.finish()
}

type LayersChangedCallback = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

#[derive(Clone)]
pub struct StatusHandle {
    mode: gtk::Label,
    device: gtk::Label,
    layer_count: gtk::Label,
    switch: gtk::Switch,
    syncing: Rc<Cell<bool>>,
    /// 图层放大预览窗口的绘制区（打开时才有）。
    preview: Rc<RefCell<Option<gtk::DrawingArea>>>,
    /// 图层列表本体，内容变化时只让它重画（不重建行控件）。
    layer_list: Rc<RefCell<Option<gtk::ListBox>>>,
    layers_changed: LayersChangedCallback,
    /// 上次重建图层列表时的结构指纹（图层数/顺序/名称/可见性）。
    structure_hash: Rc<Cell<u64>>,
    /// 上次重画缩略图时的内容指纹（每层笔数）。
    content_hash: Rc<Cell<u64>>,
}

impl StatusHandle {
    /// 刷新状态栏，并在图层结构变化时重建图层列表。
    ///
    /// 注意：重建列表会销毁并重建行内的输入框；如果调用方此刻还持有
    /// `state` 的借用，输入框的 focus-out 回调再借用就会 panic。因此这里
    /// 先在短借用内算好要不要重建，释放借用后再真正重建。
    pub fn refresh_state(&self, state: &Rc<RefCell<App>>) {
        let (needs_rebuild, needs_repaint) = {
            let app = state.borrow();
            self.update_mode(self.switch.is_active());
            self.update_device(&app);
            // 只报层数，不显示笔画数量。
            self.layer_count
                .set_text(&format!("共 {} 层", app.document.layers.len()));
            let structure = structure_key(&app);
            let content = content_key(&app);
            (
                self.structure_hash.replace(structure) != structure,
                self.content_hash.replace(content) != content,
            )
        };
        // 内容变化（落笔/擦除）只让面板重画缩略图，绝不重建行控件 —— 重建会肉眼可见地闪一下。
        if needs_rebuild {
            if let Some(preview) = self.preview.borrow().as_ref() {
                preview.queue_draw();
            }
            self.refresh_structure();
        } else if needs_repaint {
            if let Some(list) = self.layer_list.borrow().as_ref() {
                list.queue_draw();
            }
            if let Some(preview) = self.preview.borrow().as_ref() {
                preview.queue_draw();
            }
        }
    }

    /// 把结构指纹更新到当前状态。
    ///
    /// 就地改完图层行（改名、切可见性）后调用，避免紧接着触发一次列表重建 ——
    /// 重建会销毁正在编辑的输入框，既会打断输入也会带来重入风险。
    pub fn mark_structure_synced(&self, app: &App) {
        self.structure_hash.set(structure_key(app));
        self.content_hash.set(content_key(app));
    }

    pub fn refresh_structure(&self) {
        if let Some(callback) = self.layers_changed.borrow().clone() {
            callback();
        }
    }

    fn update_mode(&self, passthrough: bool) {
        if passthrough {
            self.mode
                .set_markup("<b>穿透模式</b>：输入传给下层应用，笔迹继续显示");
        } else {
            self.mode
                .set_markup("<b>标注模式</b>：笔和鼠标都在覆盖层上绘制");
        }
    }

    fn update_device(&self, app: &App) {
        let text = match app.device() {
            Some(device) => {
                let pressure = if device.pressure_available {
                    format!("{:.0}%", device.pressure * 100.0)
                } else {
                    "无".to_string()
                };
                format!(
                    "设备：{} · {} · 压感 {} · 压感轴 {}",
                    device.name,
                    device.tool.label(),
                    pressure,
                    if device.has_pressure_axis {
                        "有"
                    } else {
                        "无"
                    }
                )
            }
            None => "设备：等待笔或鼠标输入".to_string(),
        };
        self.device.set_text(&text);
    }

    pub fn set_passthrough_state(&self, enabled: bool) {
        self.syncing.set(true);
        self.switch.set_active(enabled);
        self.syncing.set(false);
        self.update_mode(enabled);
    }
}

pub struct Toolbar {
    pub window: gtk::Window,
    pub status: StatusHandle,
    pub pen_button: gtk::RadioButton,
    pub eraser_button: gtk::RadioButton,
    pub line_button: gtk::RadioButton,
    pub rect_button: gtk::RadioButton,
    pub ellipse_button: gtk::RadioButton,
    pub width_scale: gtk::Scale,
    pub gamma_scale: gtk::Scale,
    pub min_ratio_scale: gtk::Scale,
    pub layer_list: gtk::ListBox,
}

/// 工具窗向外暴露的副作用回调，由 `main` 装配（UI 层不直接碰平台 API）。
pub struct ToolbarHooks {
    /// 请求覆盖层重绘。
    pub redraw: Rc<dyn Fn()>,
    /// 切换鼠标/数位笔穿透。
    pub on_passthrough: Rc<dyn Fn(bool)>,
    /// 退出应用。
    pub on_quit: Rc<dyn Fn()>,
    /// 文档被替换（打开工程）后重建视图变换。
    pub on_document_loaded: Rc<dyn Fn()>,
    /// 截取主显示器；内部负责隐藏/恢复自身窗口。
    pub capture_screen: Rc<dyn Fn() -> Result<CaptureImage, PlatformError>>,
}

/// 图层面板用的扁平样式：名字输入框在非编辑状态下不要边框和底色。
fn install_layer_css() {
    let provider = gtk::CssProvider::new();
    if provider
        .load_from_data(
            b".layer-name { background-color: transparent; background-image: none; border-width: 0; box-shadow: none; padding-left: 2px; padding-right: 2px; }\
              .layer-eye { background-color: transparent; background-image: none; border-width: 0; box-shadow: none; padding: 3px; min-width: 0; min-height: 0; }\
              .layer-eye:hover { background-color: rgba(0, 0, 0, 0.08); border-radius: 4px; }\
              .flat-icon { background-color: transparent; background-image: none; border-width: 0; padding: 5px; min-width: 0; min-height: 0; }\
              .flat-icon:hover { background-color: rgba(0, 0, 0, 0.08); border-radius: 4px; }\
              .tool-icon { padding: 2px; min-width: 0; min-height: 0; }\
              .color-swatch { background-color: transparent; background-image: none; border-width: 0; box-shadow: none; padding: 1px; min-width: 0; min-height: 0; }",
        )
        .is_err()
    {
        log::warn!("图层面板 CSS 加载失败");
        return;
    }
    log::debug!("图层面板 CSS 已加载");
    if let Some(display) = gdk::Display::default() {
        gtk::StyleContext::add_provider_for_screen(
            &display.default_screen(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

pub fn build(
    gtk_app: &gtk::Application,
    state: Rc<RefCell<App>>,
    hooks: ToolbarHooks,
    monitor: MonitorInfo,
) -> Toolbar {
    let ToolbarHooks {
        redraw,
        on_passthrough,
        on_quit,
        on_document_loaded,
        capture_screen,
    } = hooks;
    install_layer_css();
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_application(Some(gtk_app));
    window.set_title("LInk");
    window.set_default_size(360, -1);
    window.set_keep_above(true);
    window.set_border_width(10);

    let title = gtk::Label::new(None);
    title.set_markup("<b>LInk 屏幕标注</b>");
    title.set_xalign(0.0);

    let mode_label = gtk::Label::new(None);
    mode_label.set_xalign(0.0);
    mode_label.set_line_wrap(true);
    mode_label.set_text("标注模式");

    let device_label = gtk::Label::new(None);
    device_label.set_xalign(0.0);
    device_label.set_line_wrap(true);
    device_label.set_text("设备：等待笔或鼠标输入");
    device_label.style_context().add_class("dim-label");

    let layer_count = gtk::Label::new(None);
    layer_count.set_xalign(0.0);
    layer_count.set_text("共 1 层");
    layer_count.style_context().add_class("dim-label");

    let switch = gtk::Switch::new();
    switch.set_active(false);
    switch.set_halign(gtk::Align::End);
    let switch_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let switch_title = gtk::Label::new(Some("鼠标 / 数位笔穿透（F8）"));
    switch_title.set_xalign(0.0);
    switch_row.pack_start(&switch_title, true, true, 0);
    switch_row.pack_end(&switch, false, false, 0);

    let status = StatusHandle {
        mode: mode_label.clone(),
        device: device_label.clone(),
        layer_count: layer_count.clone(),
        switch: switch.clone(),
        syncing: Rc::new(Cell::new(false)),
        preview: Rc::new(RefCell::new(None)),
        layer_list: Rc::new(RefCell::new(None)),
        layers_changed: Rc::new(RefCell::new(None)),
        structure_hash: Rc::new(Cell::new(0)),
        content_hash: Rc::new(Cell::new(0)),
    };

    let pen_button = tool_icon_button(ToolIcon::Pen, "画笔（P）");
    let eraser_button = tool_icon_button(ToolIcon::Eraser, "橡皮擦（E）");
    eraser_button.join_group(Some(&pen_button));
    let line_button = tool_icon_button(ToolIcon::Line, "直线（L）");
    line_button.join_group(Some(&pen_button));
    let rect_button = tool_icon_button(ToolIcon::Rect, "矩形（R）");
    rect_button.join_group(Some(&pen_button));
    let ellipse_button = tool_icon_button(ToolIcon::Ellipse, "椭圆（O）");
    ellipse_button.join_group(Some(&pen_button));
    pen_button.set_active(true);

    // 五个工具排成一行（图标按钮，文字只在 tooltip 里）
    let tool_row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    for button in [
        &pen_button,
        &eraser_button,
        &line_button,
        &rect_button,
        &ellipse_button,
    ] {
        button.set_hexpand(true);
        tool_row.pack_start(button, true, true, 0);
    }

    {
        let state = state.clone();
        let redraw = redraw.clone();
        pen_button.connect_toggled(move |button| {
            if button.is_active() {
                state.borrow_mut().settings.tool = Tool::Pen;
                redraw();
            }
        });
    }
    {
        let state = state.clone();
        let redraw = redraw.clone();
        eraser_button.connect_toggled(move |button| {
            if button.is_active() {
                state.borrow_mut().settings.tool = Tool::Eraser;
                redraw();
            }
        });
    }
    for (button, tool) in [
        (&line_button, Tool::Line),
        (&rect_button, Tool::Rect),
        (&ellipse_button, Tool::Ellipse),
    ] {
        let state = state.clone();
        let redraw = redraw.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                state.borrow_mut().settings.tool = tool;
                redraw();
            }
        });
    }

    // 预设色板：点一下立刻换色，不再弹确认框
    let swatches: Rc<RefCell<Vec<gtk::DrawingArea>>> = Rc::new(RefCell::new(Vec::new()));
    let color_grid = gtk::Grid::new();
    color_grid.set_column_spacing(5);
    color_grid.set_row_spacing(5);
    for (index, &(r, g, b)) in PALETTE.iter().enumerate() {
        // 用按钮包住自绘色块：按钮天然处理点击，DrawingArea 直接收 press 事件并不可靠。
        let swatch = gtk::Button::new();
        swatch.set_relief(gtk::ReliefStyle::None);
        swatch.style_context().add_class("color-swatch");
        swatch.set_tooltip_text(Some("点击立刻换色"));

        let area = gtk::DrawingArea::new();
        area.set_size_request(20, 20);
        {
            let state = state.clone();
            area.connect_draw(move |widget, cr| {
                let selected = {
                    let app = state.borrow();
                    let current = app.settings.color;
                    current.r == r && current.g == g && current.b == b
                };
                draw_color_swatch(cr, widget.allocation(), (r, g, b), selected);
                glib::Propagation::Proceed
            });
        }
        swatch.add(&area);
        {
            let state = state.clone();
            let swatches = swatches.clone();
            let redraw = redraw.clone();
            swatch.connect_clicked(move |_| {
                state.borrow_mut().settings.color = Rgba::new(r, g, b, 255);
                for area in swatches.borrow().iter() {
                    area.queue_draw();
                }
                redraw();
            });
        }
        color_grid.attach(&swatch, (index % 6) as i32, (index / 6) as i32, 1, 1);
        swatches.borrow_mut().push(area);
    }
    // 自定义颜色（需要精确取色时才弹对话框）
    let custom_button = icon_button("color-select-symbolic", "自定义颜色…");
    color_grid.attach(&custom_button, 6, 1, 1, 1);
    {
        let parent = window.clone();
        let state = state.clone();
        let swatches = swatches.clone();
        let redraw = redraw.clone();
        custom_button.connect_clicked(move |_| {
            let dialog = gtk::ColorChooserDialog::new(Some("自定义笔色"), Some(&parent));
            dialog.set_use_alpha(false);
            dialog.set_modal(true);
            {
                let current = state.borrow().settings.color;
                dialog.set_rgba(&gdk::RGBA::new(
                    current.r as f64 / 255.0,
                    current.g as f64 / 255.0,
                    current.b as f64 / 255.0,
                    1.0,
                ));
            }
            dialog.show_all();
            if dialog.run() == gtk::ResponseType::Ok {
                let color = dialog.rgba();
                state.borrow_mut().settings.color = Rgba::new(
                    (color.red() * 255.0).round().clamp(0.0, 255.0) as u8,
                    (color.green() * 255.0).round().clamp(0.0, 255.0) as u8,
                    (color.blue() * 255.0).round().clamp(0.0, 255.0) as u8,
                    255,
                );
                for area in swatches.borrow().iter() {
                    area.queue_draw();
                }
                redraw();
            }
            dismiss_dialog(&dialog);
        });
    }
    let color_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let color_label = gtk::Label::new(Some("颜色"));
    color_label.set_xalign(0.0);
    color_label.set_valign(gtk::Align::Start);
    color_row.pack_start(&color_label, false, false, 0);
    color_row.pack_start(&color_grid, false, false, 0);

    let width_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.5, 20.0, 0.1);
    width_scale.set_value(2.0);
    width_scale.set_digits(1);
    width_scale.set_hexpand(true);
    {
        let state = state.clone();
        width_scale.connect_value_changed(move |scale| {
            state.borrow_mut().settings.width_pt = scale.value();
        });
    }
    let width_row = labeled_scale("笔宽", &width_scale, "pt");

    let gamma_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.2, 3.0, 0.05);
    gamma_scale.set_value(1.0);
    gamma_scale.set_digits(2);
    gamma_scale.set_hexpand(true);
    {
        let state = state.clone();
        gamma_scale.connect_value_changed(move |scale| {
            state.borrow_mut().settings.pressure_gamma = scale.value();
        });
    }
    let gamma_row = labeled_scale("压感 gamma", &gamma_scale, "");

    let min_ratio_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 0.9, 0.05);
    min_ratio_scale.set_value(0.25);
    min_ratio_scale.set_digits(2);
    min_ratio_scale.set_hexpand(true);
    {
        let state = state.clone();
        min_ratio_scale.connect_value_changed(move |scale| {
            state.borrow_mut().settings.min_pressure_ratio = scale.value();
        });
    }
    let min_ratio_row = labeled_scale("最细比例", &min_ratio_scale, "");

    let clear_button = icon_button("edit-clear-symbolic", "清空当前层");
    {
        let state = state.clone();
        let redraw = redraw.clone();
        let status = status.clone();
        clear_button.connect_clicked(move |_| {
            if state.borrow_mut().clear_active_layer() {
                redraw();
            }
            status.refresh_state(&state);
        });
    }

    let layer_list = gtk::ListBox::new();
    layer_list.set_selection_mode(gtk::SelectionMode::Single);
    layer_list.set_vexpand(false);
    let layer_scroll = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    layer_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    layer_scroll.set_min_content_height(150);
    layer_scroll.set_shadow_type(gtk::ShadowType::In);
    layer_scroll.add(&layer_list);
    *status.layer_list.borrow_mut() = Some(layer_list.clone());

    let layer_add = icon_button("list-add-symbolic", "新建图层");
    let layer_remove = icon_button("list-remove-symbolic", "删除当前图层");
    let layer_up = icon_button("go-up-symbolic", "上移图层");
    let layer_down = icon_button("go-down-symbolic", "下移图层");
    let layer_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    layer_buttons.pack_start(&layer_add, true, true, 0);
    layer_buttons.pack_start(&layer_remove, true, true, 0);
    layer_buttons.pack_start(&layer_up, true, true, 0);
    layer_buttons.pack_start(&layer_down, true, true, 0);

    {
        let state = state.clone();
        let status = status.clone();
        let redraw = redraw.clone();
        layer_add.connect_clicked(move |_| {
            state.borrow_mut().document.add_layer_above_active();
            state.borrow_mut().mark_dirty();
            status.refresh_structure();
            status.refresh_state(&state);
            redraw();
        });
    }
    {
        let state = state.clone();
        let status = status.clone();
        let redraw = redraw.clone();
        layer_remove.connect_clicked(move |_| {
            if state.borrow_mut().document.remove_active_layer() {
                state.borrow_mut().mark_dirty();
                status.refresh_structure();
                status.refresh_state(&state);
                redraw();
            }
        });
    }
    for (button, up) in [(&layer_up, true), (&layer_down, false)] {
        let state = state.clone();
        let status = status.clone();
        let redraw = redraw.clone();
        button.connect_clicked(move |_| {
            if state.borrow_mut().document.move_active_layer(up) {
                state.borrow_mut().mark_dirty();
                status.refresh_structure();
                status.refresh_state(&state);
                redraw();
            }
        });
    }

    // ---- PS 风格图层面板：[眼睛] [缩略图] [可编辑名字] ----
    let preview_slot: Rc<RefCell<Option<(gtk::Window, gtk::DrawingArea)>>> =
        Rc::new(RefCell::new(None));
    let preview_index: Rc<Cell<usize>> = Rc::new(Cell::new(0));

    let open_preview: Rc<dyn Fn(usize)> = {
        let parent = window.clone();
        let state = state.clone();
        let status = status.clone();
        let preview_slot = preview_slot.clone();
        let preview_index = preview_index.clone();
        Rc::new(move |index: usize| {
            preview_index.set(index);
            let name = state
                .borrow()
                .document
                .layers
                .get(index)
                .map(|layer| layer.name.clone())
                .unwrap_or_default();

            if let Some((preview, area)) = preview_slot.borrow().as_ref() {
                preview.set_title(&format!("图层预览 · {name}"));
                area.queue_draw();
                preview.present();
                return;
            }

            let preview = gtk::Window::new(gtk::WindowType::Toplevel);
            preview.set_transient_for(Some(&parent));
            preview.set_title(&format!("图层预览 · {name}"));
            preview.set_default_size(900, 560);
            preview.set_keep_above(true);
            preview.set_position(gtk::WindowPosition::Center);

            let area = gtk::DrawingArea::new();
            area.set_size_request(860, 500);
            {
                let state = state.clone();
                let preview_index = preview_index.clone();
                area.connect_draw(move |widget, cr| {
                    let app = state.borrow();
                    let allocation = widget.allocation();
                    let index = preview_index.get();
                    if let Some(layer) = app.document.layers.get(index) {
                        let _ = render::draw_layer_thumbnail(
                            cr,
                            layer,
                            app.document.width_pt,
                            app.document.height_pt,
                            (allocation.width() as f64 - 2.0).max(1.0),
                            (allocation.height() as f64 - 2.0).max(1.0),
                        );
                    }
                    glib::Propagation::Proceed
                });
            }
            *status.preview.borrow_mut() = Some(area.clone());
            preview.add(&area);
            preview.show_all();

            let slot = preview_slot.clone();
            let status = status.clone();
            preview.connect_destroy(move |_| {
                *slot.borrow_mut() = None;
                *status.preview.borrow_mut() = None;
            });
            *preview_slot.borrow_mut() = Some((preview, area));
        })
    };

    {
        let state = state.clone();
        let status = status.clone();
        let status_in_rebuild = status.clone();
        let redraw = redraw.clone();
        let layer_list = layer_list.clone();
        let open_preview = open_preview.clone();
        let rebuild: Rc<dyn Fn()> = Rc::new(move || {
            status_in_rebuild.syncing.set(true);
            for row in layer_list.children() {
                layer_list.remove(&row);
            }
            let count = state.borrow().document.layers.len();
            for index in (0..count).rev() {
                let row =
                    build_layer_row(index, &state, &status_in_rebuild, &redraw, &open_preview);
                layer_list.add(&row);
                // 窗口已经显示之后再动态添加的控件默认是隐藏的，必须显式 show_all。
                row.show_all();
            }
            let display_index = {
                let app = state.borrow();
                count.saturating_sub(1 + app.document.active)
            };
            if let Some(row) = layer_list.row_at_index(display_index as i32) {
                layer_list.select_row(Some(&row));
            }
            status_in_rebuild.syncing.set(false);
        });
        *status.layers_changed.borrow_mut() = Some(rebuild.clone());
        rebuild();
    }

    {
        let state = state.clone();
        let status = status.clone();
        let redraw = redraw.clone();
        layer_list.connect_row_selected(move |_, row| {
            if status.syncing.get() {
                return;
            }
            let Some(row) = row else {
                return;
            };
            let display_index = row.index();
            if display_index < 0 {
                return;
            }
            let changed = {
                let mut app = state.borrow_mut();
                let count = app.document.layers.len();
                let Some(model_index) = count.checked_sub(1 + display_index as usize) else {
                    return;
                };
                if app.document.active == model_index {
                    false
                } else {
                    app.document.active = model_index;
                    true
                }
            };
            if changed {
                status.refresh_state(&state);
                redraw();
            }
        });
    }

    let open_action: Rc<dyn Fn()> = {
        let window = window.clone();
        let state = state.clone();
        let status = status.clone();
        let redraw = redraw.clone();
        let on_document_loaded = on_document_loaded.clone();
        Rc::new(move || {
            if state.borrow().is_dirty() && !confirm_discard(&window) {
                return;
            }
            let Some(path) = choose_xopp_open(&window) else {
                return;
            };
            match xopp::load(&path) {
                Ok(outcome) => {
                    let warnings = outcome.warnings.clone();
                    let project_path = if outcome.ignored_unsupported {
                        None
                    } else {
                        Some(path.clone())
                    };
                    state
                        .borrow_mut()
                        .replace_document(outcome.document, project_path);
                    on_document_loaded();
                    status.refresh_structure();
                    status.refresh_state(&state);
                    redraw();
                    if !warnings.is_empty() {
                        show_warnings_dialog(&window, &warnings);
                    }
                    log::info!("工程已打开: {}", path.display());
                }
                Err(error) => show_error_dialog(&window, "打开工程失败", &error.to_string()),
            }
        })
    };
    let save_action: Rc<dyn Fn()> = {
        let window = window.clone();
        let state = state.clone();
        let status = status.clone();
        Rc::new(move || {
            let existing = state.borrow().project_path().map(|path| path.to_path_buf());
            let path = match existing {
                Some(path) => path,
                None => match choose_xopp_save(&window) {
                    Some(path) => path,
                    None => return,
                },
            };
            // 注意：不能把 `state.borrow()` 写在 match 的判别式里——它的临时借用会活到整个
            // match 分支结束，分支内再 borrow_mut() 会 panic（RefCell already borrowed）。
            let saved = {
                let app = state.borrow();
                xopp::save(&path, &app.document)
            };
            match saved {
                Ok(()) => {
                    {
                        let mut app = state.borrow_mut();
                        app.mark_saved();
                        app.set_project_path(Some(path.clone()));
                    }
                    status.refresh_state(&state);
                    log::info!("工程已保存: {}", path.display());
                }
                Err(error) => show_error_dialog(&window, "保存工程失败", &error.to_string()),
            }
        })
    };
    let export_action: Rc<dyn Fn()> = {
        let window = window.clone();
        let state = state.clone();
        let status = status.clone();
        let redraw = redraw.clone();
        let capture_screen = capture_screen.clone();
        Rc::new(move || {
            let Some(choice) = choose_export_options(&window, monitor) else {
                return;
            };
            let Some(path) = choose_image_save(&window, choice.format) else {
                return;
            };
            // 导出只包含已提交的笔迹：先落草稿，再克隆文档交给纯逻辑层。
            if state.borrow_mut().finish_draft() {
                status.refresh_state(&state);
                redraw();
            }
            let document = state.borrow().document.clone();
            let rendered = match choice.background {
                ExportBackgroundChoice::Blank => {
                    export::render(&document, &monitor, ExportBackground::Blank)
                }
                ExportBackgroundChoice::Screen => match capture_screen() {
                    Ok(capture) => {
                        export::render(&document, &monitor, ExportBackground::Screen(&capture))
                    }
                    Err(error) => {
                        show_error_dialog(&window, "截取屏幕失败", &error.to_string());
                        return;
                    }
                },
            };
            match rendered.and_then(|image| export::save(&path, choice.format, &image)) {
                Ok(()) => log::info!("图片已导出: {} ({})", path.display(), choice.format.label()),
                Err(error) => show_error_dialog(&window, "导出图片失败", &error.to_string()),
            }
        })
    };
    let open_button = icon_button("document-open-symbolic", "打开工程（Ctrl+O）");
    let save_button = icon_button("document-save-symbolic", "保存工程（Ctrl+S）");
    let export_button = icon_button("document-send-symbolic", "导出图片（Ctrl+E）");
    {
        let action = open_action.clone();
        open_button.connect_clicked(move |_| action());
    }
    {
        let action = save_action.clone();
        save_button.connect_clicked(move |_| action());
    }
    {
        let action = export_action.clone();
        export_button.connect_clicked(move |_| action());
    }
    let file_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    file_row.pack_start(&open_button, true, true, 0);
    file_row.pack_start(&save_button, true, true, 0);
    file_row.pack_start(&export_button, true, true, 0);

    let layer_title = gtk::Label::new(None);
    layer_title.set_markup("<b>图层</b>");
    layer_title.set_xalign(0.0);

    let hint = gtk::Label::new(Some(
        "P 画笔 · E 橡皮擦 · L 直线 · R 矩形 · O 椭圆\n\
         F8 穿透 · Ctrl+S 保存 · Ctrl+O 打开 · Ctrl+E 导出\n\
         Shift 约束正方形 / 正圆 · 参数仅影响新笔迹",
    ));
    hint.set_xalign(0.0);
    hint.set_line_wrap(true);
    hint.style_context().add_class("dim-label");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 7);
    content.pack_start(&title, false, false, 0);
    content.pack_start(&mode_label, false, false, 0);
    content.pack_start(&switch_row, false, false, 0);
    content.pack_start(&separator(), false, false, 0);
    content.pack_start(&tool_row, false, false, 0);
    content.pack_start(&color_row, false, false, 0);
    content.pack_start(&width_row, false, false, 0);
    content.pack_start(&gamma_row, false, false, 0);
    content.pack_start(&min_ratio_row, false, false, 0);
    content.pack_start(&separator(), false, false, 0);
    content.pack_start(&clear_button, false, false, 0);
    content.pack_start(&separator(), false, false, 0);
    content.pack_start(&layer_title, false, false, 0);
    content.pack_start(&layer_count, false, false, 0);
    content.pack_start(&layer_scroll, false, false, 0);
    content.pack_start(&layer_buttons, false, false, 0);
    let layer_hint = gtk::Label::new(Some("点眼睛显示/隐藏 · 点缩略图放大预览 · 点名字可改名"));
    layer_hint.set_xalign(0.0);
    layer_hint.set_line_wrap(true);
    layer_hint.style_context().add_class("dim-label");
    content.pack_start(&layer_hint, false, false, 0);
    content.pack_start(&separator(), false, false, 0);
    content.pack_start(&file_row, false, false, 0);
    content.pack_start(&device_label, false, false, 0);
    content.pack_start(&hint, false, false, 0);
    window.add(&content);

    {
        let status = status.clone();
        let state = state.clone();
        let on_passthrough = on_passthrough.clone();
        switch.connect_active_notify(move |switch| {
            if status.syncing.get() {
                return;
            }
            let active = switch.is_active();
            on_passthrough(active);
            status.update_mode(active);
            status.refresh_state(&state);
        });
    }

    {
        let pen_button = pen_button.clone();
        let eraser_button = eraser_button.clone();
        let line_button = line_button.clone();
        let rect_button = rect_button.clone();
        let ellipse_button = ellipse_button.clone();
        let switch = switch.clone();
        let open_action = open_action.clone();
        let save_action = save_action.clone();
        let export_action = export_action.clone();
        window.connect_key_press_event(move |_, event| {
            let ctrl = event.state().contains(gdk::ModifierType::CONTROL_MASK);
            match event.keyval() {
                gdk::keys::constants::s | gdk::keys::constants::S if ctrl => {
                    save_action();
                    glib::Propagation::Stop
                }
                gdk::keys::constants::o | gdk::keys::constants::O if ctrl => {
                    open_action();
                    glib::Propagation::Stop
                }
                gdk::keys::constants::e | gdk::keys::constants::E if ctrl => {
                    export_action();
                    glib::Propagation::Stop
                }
                gdk::keys::constants::p | gdk::keys::constants::P => {
                    pen_button.set_active(true);
                    glib::Propagation::Stop
                }
                gdk::keys::constants::e | gdk::keys::constants::E => {
                    eraser_button.set_active(true);
                    glib::Propagation::Stop
                }
                gdk::keys::constants::l | gdk::keys::constants::L => {
                    line_button.set_active(true);
                    glib::Propagation::Stop
                }
                gdk::keys::constants::r | gdk::keys::constants::R => {
                    rect_button.set_active(true);
                    glib::Propagation::Stop
                }
                gdk::keys::constants::o | gdk::keys::constants::O => {
                    ellipse_button.set_active(true);
                    glib::Propagation::Stop
                }
                gdk::keys::constants::F8 => {
                    switch.set_active(!switch.is_active());
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
    }

    {
        let state = state.clone();
        let on_quit = on_quit.clone();
        window.connect_delete_event(move |window, _| {
            if state.borrow().is_dirty() && !confirm_discard(window) {
                return glib::Propagation::Stop;
            }
            on_quit();
            glib::Propagation::Stop
        });
    }

    status.refresh_state(&state);

    Toolbar {
        window,
        status,
        pen_button,
        eraser_button,
        line_button,
        rect_button,
        ellipse_button,
        width_scale,
        gamma_scale,
        min_ratio_scale,
        layer_list,
    }
}

fn labeled_scale(title: &str, scale: &gtk::Scale, suffix: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_size_request(86, -1);
    row.pack_start(&label, false, false, 0);
    row.pack_start(scale, true, true, 0);
    if !suffix.is_empty() {
        let suffix = gtk::Label::new(Some(suffix));
        suffix.style_context().add_class("dim-label");
        row.pack_end(&suffix, false, false, 0);
    }
    row
}

fn separator() -> gtk::Separator {
    gtk::Separator::new(gtk::Orientation::Horizontal)
}

fn xopp_filter() -> gtk::FileFilter {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Xournal++ 工程 (*.xopp)"));
    filter.add_pattern("*.xopp");
    filter
}

fn choose_xopp_open(parent: &gtk::Window) -> Option<PathBuf> {
    let dialog = gtk::FileChooserDialog::with_buttons(
        Some("打开 LInk 工程"),
        Some(parent),
        gtk::FileChooserAction::Open,
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("打开", gtk::ResponseType::Accept),
        ],
    );
    dialog.set_modal(true);
    let filter = xopp_filter();
    dialog.add_filter(filter);
    // GTK3 控件默认隐藏：动 run() 之前必须 show_all，否则自定义内容不会显示。
    dialog.show_all();
    let response = dialog.run();
    let path = if response == gtk::ResponseType::Accept {
        dialog.filename()
    } else {
        None
    };
    dismiss_dialog(&dialog);
    path
}

fn choose_xopp_save(parent: &gtk::Window) -> Option<PathBuf> {
    let dialog = gtk::FileChooserDialog::with_buttons(
        Some("保存 LInk 工程"),
        Some(parent),
        gtk::FileChooserAction::Save,
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("保存", gtk::ResponseType::Accept),
        ],
    );
    dialog.set_modal(true);
    dialog.set_do_overwrite_confirmation(true);
    dialog.set_current_name("未命名.xopp");
    let filter = xopp_filter();
    dialog.add_filter(filter);
    // GTK3 控件默认隐藏：动 run() 之前必须 show_all，否则自定义内容不会显示。
    dialog.show_all();
    let response = dialog.run();
    let path = if response == gtk::ResponseType::Accept {
        dialog.filename()
    } else {
        None
    };
    dismiss_dialog(&dialog);
    let mut path = path?;
    let has_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xopp"));
    if !has_extension {
        path.set_extension("xopp");
    }
    Some(path)
}

/// `gtk_dialog_run()` 返回后必须显式销毁对话框。
///
/// 运行期间 GTK 会拦截 delete-event，此时只调用 `close()` 窗口仍会留在屏幕上，
/// 进而被「导出当前屏幕」一起拍进图片里。这里先隐藏再销毁。
fn dismiss_dialog(dialog: &impl IsA<gtk::Widget>) {
    dialog.hide();
    // SAFETY: 调用方随即丢弃这个对话框的 Rust 包装，不再使用。
    unsafe { dialog.destroy() };
}

fn visibility_icon(visible: bool) -> &'static str {
    if visible {
        "view-reveal-symbolic"
    } else {
        "view-conceal-symbolic"
    }
}

/// 延后到当前信号处理结束再刷新。
///
/// 图层行的按钮/输入框回调里会触发列表重建，如果在自己的信号处理中直接销毁控件，
/// GTK 会处在“一边派发事件一边析构”的危险状态，所以推迟到下一轮空闲。
fn defer_refresh(state: Rc<RefCell<App>>, status: StatusHandle) {
    glib::idle_add_local_once(move || {
        status.refresh_state(&state);
    });
}

/// 一行 PS 风格图层项：[眼睛] [缩略图] [可编辑名字]。
fn build_layer_row(
    index: usize,
    state: &Rc<RefCell<App>>,
    status: &StatusHandle,
    redraw: &Rc<dyn Fn()>,
    open_preview: &Rc<dyn Fn(usize)>,
) -> gtk::ListBoxRow {
    let (name, visible) = {
        let app = state.borrow();
        match app.document.layers.get(index) {
            Some(layer) => (layer.name.clone(), layer.visible),
            None => (String::new(), true),
        }
    };

    let row = gtk::ListBoxRow::new();
    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row_box.set_margin_start(4);
    row_box.set_margin_end(4);
    row_box.set_margin_top(3);
    row_box.set_margin_bottom(3);

    // 缩略图（点击放大预览）
    let thumb = gtk::DrawingArea::new();
    thumb.set_size_request(56, 32);
    thumb.set_tooltip_text(Some("点击放大预览"));
    {
        let state = state.clone();
        thumb.connect_draw(move |widget, cr| {
            let app = state.borrow();
            let allocation = widget.allocation();
            if let Some(layer) = app.document.layers.get(index) {
                let _ = render::draw_layer_thumbnail(
                    cr,
                    layer,
                    app.document.width_pt,
                    app.document.height_pt,
                    (allocation.width() as f64 - 2.0).max(1.0),
                    (allocation.height() as f64 - 2.0).max(1.0),
                );
            }
            glib::Propagation::Proceed
        });
    }
    thumb.add_events(gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::BUTTON_RELEASE_MASK);
    {
        let open_preview = open_preview.clone();
        thumb.connect_button_press_event(move |_, _| {
            open_preview(index);
            glib::Propagation::Stop
        });
    }
    thumb.set_opacity(if visible { 1.0 } else { 0.35 });
    let thumb_frame = gtk::Frame::new(None);
    thumb_frame.set_shadow_type(gtk::ShadowType::In);
    thumb_frame.add(&thumb);

    // 眼睛：显示 / 隐藏（普通按钮，状态由模型决定，避免切换按钮的选中底色）
    let eye = gtk::Button::new();
    eye.set_relief(gtk::ReliefStyle::None);
    eye.style_context().add_class("layer-eye");
    eye.set_image(Some(&gtk::Image::from_icon_name(
        Some(visibility_icon(visible)),
        gtk::IconSize::Menu,
    )));
    eye.set_tooltip_text(Some(if visible {
        "隐藏图层"
    } else {
        "显示图层"
    }));
    {
        let state = state.clone();
        let status = status.clone();
        let redraw = redraw.clone();
        let thumb = thumb.clone();
        eye.connect_clicked(move |button| {
            let visible = !state
                .borrow()
                .document
                .layers
                .get(index)
                .map(|layer| layer.visible)
                .unwrap_or(true);
            let changed = state
                .borrow_mut()
                .document
                .set_layer_visible(index, visible);
            if !changed {
                return;
            }
            button.set_image(Some(&gtk::Image::from_icon_name(
                Some(visibility_icon(visible)),
                gtk::IconSize::Menu,
            )));
            button.set_tooltip_text(Some(if visible {
                "隐藏图层"
            } else {
                "显示图层"
            }));
            thumb.set_opacity(if visible { 1.0 } else { 0.35 });
            // 可见性不写入 xopp，所以不改工程 dirty 状态，只刷新画面。
            redraw();
            status.mark_structure_synced(&state.borrow());
            defer_refresh(state.clone(), status.clone());
        });
    }

    // 名字：可直接编辑
    let entry = gtk::Entry::new();
    entry.set_text(&name);
    entry.set_has_frame(false);
    entry.style_context().add_class("layer-name");
    entry.set_width_chars(10);
    entry.set_xalign(0.0);
    entry.set_tooltip_text(Some("点击可修改图层名"));
    {
        let state = state.clone();
        let status = status.clone();
        let commit: Rc<dyn Fn(&gtk::Entry)> = Rc::new(move |entry: &gtk::Entry| {
            let text = entry.text().to_string();
            let renamed = {
                let mut app = state.borrow_mut();
                if app.document.rename_layer(index, &text) {
                    app.mark_dirty();
                    true
                } else {
                    false
                }
            };
            if renamed {
                log::info!("图层重命名: {}", text.trim());
                // 行内输入框已经显示新名字，不需要重建列表；同步指纹避免重建打断输入。
                status.mark_structure_synced(&state.borrow());
                defer_refresh(state.clone(), status.clone());
            } else if let Some(current) = state
                .borrow()
                .document
                .layers
                .get(index)
                .map(|layer| layer.name.clone())
            {
                if current != text {
                    entry.set_text(&current);
                }
            }
        });
        let commit_activate = commit.clone();
        entry.connect_activate(move |entry| commit_activate(entry));
        entry.connect_focus_out_event(move |entry, _| {
            commit(entry);
            glib::Propagation::Proceed
        });
    }

    row_box.pack_start(&eye, false, false, 0);
    row_box.pack_start(&thumb_frame, false, false, 0);
    row_box.pack_start(&entry, true, true, 0);
    row.add(&row_box);
    row
}

/// 预设笔色。点一下立刻生效，不需要确认。
const PALETTE: [(u8, u8, u8); 12] = [
    (0, 0, 0),
    (90, 90, 90),
    (220, 40, 40),
    (240, 140, 30),
    (240, 210, 40),
    (60, 170, 80),
    (40, 170, 190),
    (50, 110, 230),
    (130, 70, 200),
    (210, 60, 160),
    (140, 90, 50),
    (255, 255, 255),
];

/// 扁平图标按钮：只有图标，文字放 tooltip。
fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_relief(gtk::ReliefStyle::None);
    button.style_context().add_class("flat-icon");
    button.set_image(Some(&gtk::Image::from_icon_name(
        Some(icon),
        gtk::IconSize::Button,
    )));
    button.set_tooltip_text(Some(tooltip));
    button
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolIcon {
    Pen,
    Eraser,
    Line,
    Rect,
    Ellipse,
}

/// 工具按钮：自绘图标（主题里没有画笔/橡皮的通用符号），选中态沿用主题高亮。
fn tool_icon_button(kind: ToolIcon, tooltip: &str) -> gtk::RadioButton {
    let button = gtk::RadioButton::new();
    button.set_mode(false);
    button.set_relief(gtk::ReliefStyle::None);
    button.style_context().add_class("tool-icon");
    button.set_tooltip_text(Some(tooltip));
    let area = gtk::DrawingArea::new();
    area.set_size_request(26, 26);
    area.connect_draw(move |widget, cr| {
        let context = widget.style_context();
        let color = context.color(context.state());
        draw_tool_icon(cr, kind, widget.allocation(), color);
        glib::Propagation::Proceed
    });
    button.add(&area);
    button
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, width: f64, height: f64, radius: f64) {
    let r = radius.min(width / 2.0).min(height / 2.0).max(0.0);
    cr.new_sub_path();
    cr.arc(x + width - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(
        x + width - r,
        y + height - r,
        r,
        0.0,
        std::f64::consts::FRAC_PI_2,
    );
    cr.arc(
        x + r,
        y + height - r,
        r,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    cr.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::PI,
        1.5 * std::f64::consts::PI,
    );
    cr.close_path();
}

fn draw_tool_icon(
    cr: &cairo::Context,
    kind: ToolIcon,
    allocation: gtk::Allocation,
    color: gdk::RGBA,
) {
    let size = allocation.width().min(allocation.height()) as f64;
    let pad = size * 0.20;
    let inner = size - pad * 2.0;
    let _ = cr.save();
    cr.set_source_rgba(color.red(), color.green(), color.blue(), color.alpha());
    cr.set_line_width((size * 0.085).max(1.4));
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);

    match kind {
        ToolIcon::Pen => {
            // 铅笔：斜放的细长笔杆 + 笔尖三角
            let _ = cr.save();
            cr.translate(size / 2.0, size / 2.0);
            cr.rotate(-std::f64::consts::FRAC_PI_4);
            let body_w = inner * 0.44;
            let body_h = inner * 0.98;
            rounded_rect(
                cr,
                -body_w / 2.0,
                -body_h / 2.0,
                body_w,
                body_h * 0.72,
                body_w * 0.18,
            );
            let _ = cr.stroke();
            cr.move_to(-body_w / 2.0, body_h * 0.36 - body_w * 0.18);
            cr.line_to(0.0, body_h / 2.0);
            cr.line_to(body_w / 2.0, body_h * 0.36 - body_w * 0.18);
            cr.close_path();
            let _ = cr.fill();
            let _ = cr.restore();
        }
        ToolIcon::Eraser => {
            // 斜放的橡皮
            cr.translate(size / 2.0, size * 0.56);
            cr.rotate(-0.65);
            let w = inner * 0.92;
            let h = inner * 0.46;
            rounded_rect(cr, -w / 2.0, -h / 2.0, w, h, h * 0.3);
            let _ = cr.stroke_preserve();
            cr.set_line_width((size * 0.05).max(1.0));
            let _ = cr.save();
            cr.clip();
            cr.move_to(-w * 0.15, -h);
            cr.line_to(-w * 0.15, h);
            let _ = cr.stroke();
            let _ = cr.restore();
        }
        ToolIcon::Line => {
            cr.move_to(pad + inner * 0.05, size - pad - inner * 0.05);
            cr.line_to(size - pad - inner * 0.05, pad + inner * 0.05);
            let _ = cr.stroke();
            for (x, y) in [
                (pad + inner * 0.05, size - pad - inner * 0.05),
                (size - pad - inner * 0.05, pad + inner * 0.05),
            ] {
                cr.new_sub_path();
                cr.arc(x, y, size * 0.075, 0.0, std::f64::consts::TAU);
                let _ = cr.fill();
            }
        }
        ToolIcon::Rect => {
            rounded_rect(cr, pad, pad, inner, inner, size * 0.08);
            let _ = cr.stroke();
        }
        ToolIcon::Ellipse => {
            let _ = cr.save();
            cr.translate(size / 2.0, size / 2.0);
            cr.scale(1.0, 0.72);
            cr.new_sub_path();
            cr.arc(0.0, 0.0, inner * 0.56, 0.0, std::f64::consts::TAU);
            let _ = cr.restore();
            let _ = cr.stroke();
        }
    }
    let _ = cr.restore();
}

fn draw_color_swatch(
    cr: &cairo::Context,
    allocation: gtk::Allocation,
    rgb: (u8, u8, u8),
    selected: bool,
) {
    let width = allocation.width() as f64;
    let height = allocation.height() as f64;
    let inset = 1.5;
    rounded_rect(
        cr,
        inset,
        inset,
        width - inset * 2.0,
        height - inset * 2.0,
        4.0,
    );
    cr.set_source_rgb(
        rgb.0 as f64 / 255.0,
        rgb.1 as f64 / 255.0,
        rgb.2 as f64 / 255.0,
    );
    let _ = cr.fill_preserve();
    if selected {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.75);
        cr.set_line_width(2.0);
    } else {
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.20);
        cr.set_line_width(1.0);
    }
    let _ = cr.stroke();
}

fn confirm_discard(parent: &gtk::Window) -> bool {
    let dialog = gtk::MessageDialog::new(
        Some(parent),
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Question,
        gtk::ButtonsType::YesNo,
        "当前工程有未保存的修改，确定要放弃吗？",
    );
    dialog.set_title("LInk");
    // GTK3 控件默认隐藏：动 run() 之前必须 show_all，否则自定义内容不会显示。
    dialog.show_all();
    let response = dialog.run();
    dismiss_dialog(&dialog);
    response == gtk::ResponseType::Yes
}

fn show_error_dialog(parent: &gtk::Window, title: &str, message: &str) {
    let dialog = gtk::MessageDialog::new(
        Some(parent),
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Error,
        gtk::ButtonsType::Ok,
        message,
    );
    dialog.set_title(title);
    dialog.run();
    dismiss_dialog(&dialog);
}

fn show_warnings_dialog(parent: &gtk::Window, warnings: &[String]) {
    let text = warnings.join("\n\n");
    let dialog = gtk::MessageDialog::new(
        Some(parent),
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Warning,
        gtk::ButtonsType::Ok,
        &text,
    );
    dialog.set_title("工程已加载，但有提示");
    dialog.run();
    dismiss_dialog(&dialog);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportBackgroundChoice {
    /// 白底，只保留笔迹（适合贴到文档里）。
    Blank,
    /// 真实屏幕背景，笔迹叠在上面。
    Screen,
}

#[derive(Debug, Clone, Copy)]
struct ExportChoice {
    format: ImageFormat,
    background: ExportBackgroundChoice,
}

/// 导出选项对话框：图片格式 + 背景来源。取消返回 `None`。
fn choose_export_options(parent: &gtk::Window, monitor: MonitorInfo) -> Option<ExportChoice> {
    let dialog = gtk::Dialog::with_buttons(
        Some("导出图片"),
        Some(parent),
        gtk::DialogFlags::MODAL,
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("选择保存位置", gtk::ResponseType::Accept),
        ],
    );
    dialog.set_default_response(gtk::ResponseType::Accept);

    let format_combo = gtk::ComboBoxText::new();
    format_combo.append_text("PNG（无损，推荐）");
    format_combo.append_text("JPEG（体积更小）");
    format_combo.set_active(Some(0));

    let format_label = gtk::Label::new(Some("图片格式"));
    format_label.set_xalign(0.0);
    format_label.set_size_request(76, -1);
    let format_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    format_row.pack_start(&format_label, false, false, 0);
    format_row.pack_start(&format_combo, true, true, 0);

    let background_label = gtk::Label::new(Some("背景"));
    background_label.set_xalign(0.0);
    background_label.style_context().add_class("dim-label");

    let blank_radio = gtk::RadioButton::with_label("空白白底（只保留笔迹）");
    blank_radio.set_active(true);
    let screen_radio =
        gtk::RadioButton::with_label_from_widget(&blank_radio, "当前屏幕（含真实背景）");

    let size_text = gtk::Label::new(None);
    size_text.set_xalign(0.0);
    size_text.set_line_wrap(true);
    size_text.style_context().add_class("dim-label");
    size_text.set_text(&format!(
        "输出尺寸：{}×{} 物理像素（与屏幕位置一致）",
        monitor.physical.width, monitor.physical.height
    ));

    let content = dialog.content_area();
    content.set_border_width(12);
    content.set_spacing(8);
    content.pack_start(&format_row, false, false, 0);
    content.pack_start(&separator(), false, false, 0);
    content.pack_start(&background_label, false, false, 0);
    content.pack_start(&blank_radio, false, false, 0);
    content.pack_start(&screen_radio, false, false, 0);
    content.pack_start(&size_text, false, false, 0);

    // GTK3 控件默认隐藏：动 run() 之前必须 show_all，否则自定义内容不会显示。
    dialog.show_all();
    let response = dialog.run();
    let choice = if response == gtk::ResponseType::Accept {
        let format = match format_combo.active() {
            Some(1) => ImageFormat::Jpeg,
            _ => ImageFormat::Png,
        };
        let background = if screen_radio.is_active() {
            ExportBackgroundChoice::Screen
        } else {
            ExportBackgroundChoice::Blank
        };
        Some(ExportChoice { format, background })
    } else {
        None
    };
    dismiss_dialog(&dialog);
    choice
}

fn image_filter(format: ImageFormat) -> gtk::FileFilter {
    let filter = gtk::FileFilter::new();
    match format {
        ImageFormat::Png => {
            filter.set_name(Some("PNG 图片 (*.png)"));
            filter.add_pattern("*.png");
        }
        ImageFormat::Jpeg => {
            filter.set_name(Some("JPEG 图片 (*.jpg, *.jpeg)"));
            filter.add_pattern("*.jpg");
            filter.add_pattern("*.jpeg");
        }
    }
    filter
}

fn choose_image_save(parent: &gtk::Window, format: ImageFormat) -> Option<PathBuf> {
    let dialog = gtk::FileChooserDialog::with_buttons(
        Some("导出图片"),
        Some(parent),
        gtk::FileChooserAction::Save,
        &[
            ("取消", gtk::ResponseType::Cancel),
            ("导出", gtk::ResponseType::Accept),
        ],
    );
    dialog.set_modal(true);
    dialog.set_do_overwrite_confirmation(true);
    dialog.set_current_name(&format!("屏幕标注.{}", format.extension()));
    dialog.add_filter(image_filter(format));
    // GTK3 控件默认隐藏：动 run() 之前必须 show_all，否则自定义内容不会显示。
    dialog.show_all();
    let response = dialog.run();
    let path = if response == gtk::ResponseType::Accept {
        dialog.filename()
    } else {
        None
    };
    dismiss_dialog(&dialog);
    let mut path = path?;
    let matches_format = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| match format {
            ImageFormat::Png => extension.eq_ignore_ascii_case("png"),
            ImageFormat::Jpeg => {
                extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
            }
        });
    if !matches_format {
        path.set_extension(format.extension());
    }
    Some(path)
}
