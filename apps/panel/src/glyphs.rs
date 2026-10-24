//! 全部图标都用 egui 的 Painter 手绘：不依赖图标字体，跨平台一致。
//!
//! 约定：所有坐标都用 `s = 图标半边长` 的倍数书写，因此同一套图形可以在
//! 20px 的行内小图标和 42px 的工具按钮上复用。实心剪影用于「铅笔 / 橡皮 / 文件夹」，
//! 线性轮廓用于「矩形 / 椭圆 / 直线 / 软盘 / 垃圾桶」。

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Pen,
    Eraser,
    Line,
    Rect,
    Ellipse,
    Plus,
    Minus,
    Up,
    Down,
    EyeOpen,
    EyeClosed,
    Folder,
    Save,
    Export,
    Trash,
    Close,
}

/// 绕 `center` 旋转 `angle` 弧度。
fn rotate(point: Pos2, center: Pos2, angle: f32) -> Pos2 {
    let dx = point.x - center.x;
    let dy = point.y - center.y;
    let (sin, cos) = angle.sin_cos();
    Pos2::new(
        center.x + dx * cos - dy * sin,
        center.y + dx * sin + dy * cos,
    )
}

pub fn paint(painter: &Painter, rect: Rect, glyph: Glyph, color: Color32) {
    let size = rect.width().min(rect.height());
    let c = rect.center();
    let s = size * 0.5;
    let stroke = Stroke::new((size * 0.085).max(1.25), color);

    match glyph {
        Glyph::Pen => {
            // 铅笔：沿 -45° 轴线参数化 —— 笔尾、笔身、留缺口、笔尖。
            let dir = Vec2::angled(-std::f32::consts::FRAC_PI_4); // 指向左下
            let normal = Vec2::new(-dir.y, dir.x);
            let half_w = 0.21 * s;
            let tail = c - dir * 0.60 * s;
            let neck = c + dir * 0.22 * s;
            let tip = c + dir * 0.76 * s;

            // 笔身（实心矩形，笔尾处切一个小口表示橡皮箍）
            let body_cut = tail + dir * 0.16 * s;
            painter.add(Shape::convex_polygon(
                vec![
                    body_cut - normal * half_w,
                    body_cut + normal * half_w,
                    neck + normal * half_w,
                    neck - normal * half_w,
                ],
                color,
                Stroke::NONE,
            ));
            // 橡皮箍：笔尾单独一小块，和笔身之间留缝
            painter.add(Shape::convex_polygon(
                vec![
                    tail - normal * half_w,
                    tail + normal * half_w,
                    tail + dir * 0.10 * s + normal * half_w,
                    tail + dir * 0.10 * s - normal * half_w,
                ],
                color,
                Stroke::NONE,
            ));
            // 笔尖：从笔身宽度收窄到一个尖，看起来像削出来的铅笔头
            let tip_base = 0.62 * half_w;
            painter.add(Shape::convex_polygon(
                vec![
                    neck + dir * 0.08 * s + normal * tip_base,
                    neck + dir * 0.08 * s - normal * tip_base,
                    tip,
                ],
                color,
                Stroke::NONE,
            ));
        }
        Glyph::Eraser => {
            // 橡皮：斜放的方块，后半段填充、前半段只描边，看起来像被用过一半。
            let angle = -0.62;
            let quad = |x0: f32, x1: f32| -> Vec<Pos2> {
                [(x0, -0.34), (x1, -0.34), (x1, 0.34), (x0, 0.34)]
                    .iter()
                    .map(|(x, y)| rotate(Pos2::new(c.x + x * s, c.y + y * s), c, angle))
                    .collect()
            };
            painter.add(Shape::convex_polygon(
                quad(-0.86, -0.10),
                color,
                Stroke::NONE,
            ));
            painter.add(Shape::closed_line(quad(-0.10, 0.70), stroke));
        }
        Glyph::Line => {
            let a = Pos2::new(c.x - 0.60 * s, c.y + 0.60 * s);
            let b = Pos2::new(c.x + 0.60 * s, c.y - 0.60 * s);
            painter.line_segment([a, b], stroke);
            painter.circle_filled(a, 0.15 * s, color);
            painter.circle_filled(b, 0.15 * s, color);
        }
        Glyph::Rect => {
            painter.rect_stroke(
                Rect::from_center_size(c, Vec2::new(size * 0.88, size * 0.70)),
                egui::CornerRadius::same((size * 0.10) as u8),
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        Glyph::Ellipse => {
            let points: Vec<Pos2> = (0..=40)
                .map(|i| {
                    let t = i as f32 / 40.0 * std::f32::consts::TAU;
                    Pos2::new(c.x + t.cos() * 0.86 * s, c.y + t.sin() * 0.62 * s)
                })
                .collect();
            painter.add(Shape::closed_line(points, stroke));
        }
        Glyph::Plus => {
            painter.line_segment(
                [
                    Pos2::new(c.x - 0.58 * s, c.y),
                    Pos2::new(c.x + 0.58 * s, c.y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x, c.y - 0.58 * s),
                    Pos2::new(c.x, c.y + 0.58 * s),
                ],
                stroke,
            );
        }
        Glyph::Minus => {
            painter.line_segment(
                [
                    Pos2::new(c.x - 0.58 * s, c.y),
                    Pos2::new(c.x + 0.58 * s, c.y),
                ],
                stroke,
            );
        }
        Glyph::Up | Glyph::Down => {
            let dir = if matches!(glyph, Glyph::Up) {
                -1.0
            } else {
                1.0
            };
            let tip = Pos2::new(c.x, c.y + 0.50 * s * dir);
            let left = Pos2::new(c.x - 0.42 * s, c.y - 0.30 * s * dir);
            let right = Pos2::new(c.x + 0.42 * s, c.y - 0.30 * s * dir);
            painter.add(Shape::line(vec![left, tip, right], stroke));
        }
        Glyph::EyeOpen | Glyph::EyeClosed => {
            // 眼形：上下两段弧组成梭形，中间实心瞳孔。
            let w = 0.92 * s;
            let h = 0.52 * s;
            let arc = |sign: f32| -> Vec<Pos2> {
                (0..=16)
                    .map(|i| {
                        let t = i as f32 / 16.0;
                        let x = c.x - w + 2.0 * w * t;
                        let y = c.y + sign * (std::f32::consts::PI * t).sin() * h;
                        Pos2::new(x, y)
                    })
                    .collect()
            };
            painter.add(Shape::line(arc(-1.0), stroke));
            painter.add(Shape::line(arc(1.0), stroke));
            match glyph {
                Glyph::EyeOpen => {
                    painter.circle_filled(c, 0.26 * s, color);
                }
                _ => {
                    let slash = Stroke::new(stroke.width * 1.25, color);
                    painter.line_segment(
                        [
                            Pos2::new(c.x - 0.72 * s, c.y - 0.62 * s),
                            Pos2::new(c.x + 0.72 * s, c.y + 0.62 * s),
                        ],
                        slash,
                    );
                }
            }
        }
        Glyph::Folder => {
            // 实心文件夹：左上角带一个标签页凸起。
            let left = c.x - 0.92 * s;
            let right = c.x + 0.92 * s;
            let top = c.y - 0.62 * s;
            let tab_bottom = c.y - 0.34 * s;
            let tab_right = c.x - 0.24 * s;
            let bottom = c.y + 0.66 * s;
            let shape = vec![
                Pos2::new(left, tab_bottom),
                Pos2::new(left + 0.06 * s, top),
                Pos2::new(tab_right, top),
                Pos2::new(tab_right + 0.20 * s, tab_bottom),
                Pos2::new(right, tab_bottom),
                Pos2::new(right, bottom - 0.12 * s),
                Pos2::new(right - 0.10 * s, bottom),
                Pos2::new(left + 0.10 * s, bottom),
                Pos2::new(left, bottom - 0.12 * s),
            ];
            painter.add(Shape::convex_polygon(shape, color, Stroke::NONE));
        }
        Glyph::Save => {
            // 软盘：外框 + 上方快门 + 下方标签。
            let body = Rect::from_center_size(c, Vec2::new(size * 0.88, size * 0.88));
            painter.rect_stroke(
                body,
                egui::CornerRadius::same((size * 0.10) as u8),
                stroke,
                egui::StrokeKind::Middle,
            );
            let shutter = Rect::from_min_size(
                Pos2::new(body.left() + body.width() * 0.42, body.top()),
                Vec2::new(body.width() * 0.40, body.height() * 0.30),
            );
            painter.rect_filled(shutter, 1.0, color);
            let label = Rect::from_min_size(
                Pos2::new(
                    body.left() + body.width() * 0.20,
                    body.bottom() - body.height() * 0.36,
                ),
                Vec2::new(body.width() * 0.60, body.height() * 0.30),
            );
            painter.rect_stroke(
                label,
                egui::CornerRadius::same(1),
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        Glyph::Export => {
            // 导出：一个画框 + 向右上离开的箭头。
            let frame = Rect::from_center_size(
                Pos2::new(c.x - 0.10 * s, c.y + 0.12 * s),
                Vec2::new(size * 0.72, size * 0.72),
            );
            painter.rect_stroke(
                frame,
                egui::CornerRadius::same((size * 0.08) as u8),
                stroke,
                egui::StrokeKind::Middle,
            );
            let from = Pos2::new(c.x + 0.02 * s, c.y + 0.02 * s);
            let to = Pos2::new(c.x + 0.78 * s, c.y - 0.78 * s);
            painter.line_segment([from, to], stroke);
            let head = 0.26 * s;
            painter.line_segment([to, Pos2::new(to.x - head, to.y + 0.02 * s)], stroke);
            painter.line_segment([to, Pos2::new(to.x - 0.02 * s, to.y + head)], stroke);
        }
        Glyph::Trash => {
            // 垃圾桶：盖子 + 提手 + 略微收口的桶身 + 两条竖线。
            let top = c.y - 0.52 * s;
            let bottom = c.y + 0.72 * s;
            let half_top = 0.56 * s;
            let half_bottom = 0.44 * s;
            painter.line_segment(
                [
                    Pos2::new(c.x - half_top, top),
                    Pos2::new(c.x + half_top, top),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x - 0.18 * s, top),
                    Pos2::new(c.x - 0.18 * s, top - 0.18 * s),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x - 0.18 * s, top - 0.18 * s),
                    Pos2::new(c.x + 0.18 * s, top - 0.18 * s),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x + 0.18 * s, top - 0.18 * s),
                    Pos2::new(c.x + 0.18 * s, top),
                ],
                stroke,
            );
            painter.add(Shape::line(
                vec![
                    Pos2::new(c.x - half_top, top),
                    Pos2::new(c.x - half_bottom, bottom),
                    Pos2::new(c.x + half_bottom, bottom),
                    Pos2::new(c.x + half_top, top),
                ],
                stroke,
            ));
            for dx in [-0.18, 0.18] {
                painter.line_segment(
                    [
                        Pos2::new(c.x + dx * s, top + 0.24 * s),
                        Pos2::new(c.x + dx * 0.8 * s, bottom - 0.18 * s),
                    ],
                    Stroke::new(stroke.width * 0.8, color),
                );
            }
        }
        Glyph::Close => {
            painter.line_segment(
                [
                    Pos2::new(c.x - 0.48 * s, c.y - 0.48 * s),
                    Pos2::new(c.x + 0.48 * s, c.y + 0.48 * s),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(c.x + 0.48 * s, c.y - 0.48 * s),
                    Pos2::new(c.x - 0.48 * s, c.y + 0.48 * s),
                ],
                stroke,
            );
        }
    }
}
