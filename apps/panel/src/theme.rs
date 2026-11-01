//! 面板的视觉常量与小部件绘制助手（自绘，不依赖默认控件外观）。

use egui::{Color32, CornerRadius, FontId, Rect, Vec2};

pub const PANEL_BG: Color32 = Color32::from_rgb(0xFA, 0xFA, 0xFC);
pub const CARD_BG: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
pub const BORDER: Color32 = Color32::from_rgb(0xE7, 0xE7, 0xEE);
pub const TEXT: Color32 = Color32::from_rgb(0x1C, 0x1C, 0x22);
pub const DIM: Color32 = Color32::from_rgb(0x8A, 0x8A, 0x95);
pub const ACCENT: Color32 = Color32::from_rgb(0x51, 0x47, 0xE5);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0xEE, 0xEC, 0xFD);
pub const DANGER: Color32 = Color32::from_rgb(0xD9, 0x3B, 0x3B);

pub const RADIUS_PANEL: u8 = 14;
pub const RADIUS_ROW: u8 = 8;

pub fn body() -> FontId {
    FontId::proportional(13.0)
}

pub fn small() -> FontId {
    FontId::proportional(11.0)
}

/// 圆角矩形填充。
pub fn fill_rounded(painter: &egui::Painter, rect: Rect, radius: u8, color: Color32) {
    painter.rect_filled(rect, CornerRadius::same(radius), color);
}

/// 图标按钮（自绘图标），返回是否被点击。
pub fn icon_button(
    ui: &mut egui::Ui,
    glyph: crate::glyphs::Glyph,
    size: Vec2,
    selected: bool,
    tooltip: &str,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = response.hovered();
    let painter = ui.painter();
    let bg = if selected {
        ACCENT_SOFT
    } else if hovered {
        Color32::from_rgb(0xF0, 0xF0, 0xF5)
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        fill_rounded(painter, rect.shrink(1.0), 7, bg);
    }
    let color = if selected { ACCENT } else { TEXT };
    crate::glyphs::paint(painter, rect.shrink(rect.width() * 0.16), glyph, color);
    response.on_hover_text(tooltip).clicked()
}
