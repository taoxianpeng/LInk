//! 与平台无关的绘制抽象：把「几何」和「光栅化后端」分开。
//!
//! 上层（[`crate::render`]、[`crate::cache`]、[`crate::export`]、[`crate::ui::overlay`]）
//! 只依赖这里的 [`Path2`] / [`Canvas`] / [`Surface`]，不认识 cairo，也不认识 tiny-skia。
//! 这样同一份笔迹几何能在 Linux（cairo + GTK 窗口）和 Windows（tiny-skia + 分层窗口）上
//! 得到一致的结果。
//!
//! **坐标约定**：所有绘制坐标都是**表面设备像素**。高 DPI 屏上由调用方用
//! [`crate::view::ViewTransform::to_physical_pixels`] 把页面 pt 直接换算到物理像素，
//! 后端不再持有设备缩放（cairo 的 `set_device_scale` 在 Windows 上没有对应物，
//! 隐式缩放正是两边最容易漂移的地方）。

use std::any::Any;
use std::fmt;

#[cfg(target_os = "linux")]
mod cairo_backend;
#[cfg(target_os = "windows")]
mod skia_backend;

#[cfg(target_os = "linux")]
pub use cairo_backend::{CairoCanvas as PlatformCanvas, CairoSurface as PlatformSurface};
#[cfg(target_os = "windows")]
pub use skia_backend::{SkiaCanvas as PlatformCanvas, SkiaSurface as PlatformSurface};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blend {
    /// 直接替换目标像素（包括透明像素）。缓存贴图与原子合成的最后一步必须用它，
    /// 否则上一帧的图形会残留在屏幕上（README 第 16 条）。
    Source,
    /// 正常叠加。
    Over,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Self = Self::new(0, 0, 0, 0);
    pub const WHITE: Self = Self::new(255, 255, 255, 255);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_rgba_f64(r: f64, g: f64, b: f64, a: f64) -> Self {
        let to_u8 = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        Self::new(to_u8(r), to_u8(g), to_u8(b), to_u8(a))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    Miter,
    Round,
    Bevel,
}

/// 后端无关的路径构建器。坐标是调用方坐标（绘制时会叠加当前变换）。
#[derive(Debug, Clone, Default)]
pub struct PathBuilder2 {
    ops: Vec<PathOp>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum PathOp {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    Close,
    /// 独立的圆形子路径：等价于 cairo 的 `new_sub_path(); arc(..)`，
    /// 不会把当前点连到圆弧起点。
    Circle(f64, f64, f64),
}

impl PathBuilder2 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn move_to(&mut self, x: f64, y: f64) {
        self.ops.push(PathOp::MoveTo(x, y));
    }

    pub fn line_to(&mut self, x: f64, y: f64) {
        self.ops.push(PathOp::LineTo(x, y));
    }

    pub fn close(&mut self) {
        self.ops.push(PathOp::Close);
    }

    /// 追加一条独立的圆形子路径（不连接当前点）。
    pub fn circle(&mut self, x: f64, y: f64, radius: f64) {
        self.ops.push(PathOp::Circle(x, y, radius));
    }

    pub fn rect(&mut self, x: f64, y: f64, width: f64, height: f64) {
        self.move_to(x, y);
        self.line_to(x + width, y);
        self.line_to(x + width, y + height);
        self.line_to(x, y + height);
        self.close();
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn finish(self) -> Option<Path2> {
        if self.ops.is_empty() {
            None
        } else {
            Some(Path2 { ops: self.ops })
        }
    }
}

/// 序列化后的路径。后端负责把它翻译成自己的路径类型。
#[derive(Debug, Clone)]
pub struct Path2 {
    ops: Vec<PathOp>,
}

impl Path2 {
    pub(crate) fn ops(&self) -> &[PathOp] {
        &self.ops
    }
}

/// 光栅化画布。
///
/// 状态（变换 / 颜色 / 线宽 / 端点 / 虚线 / 裁剪）是隐式的，`save`/`restore` 压栈。
/// 这与 cairo 的模型一致，tiny-skia 后端在内部维护等价的状态栈。
pub trait Canvas {
    fn save(&mut self);
    fn restore(&mut self);

    /// 平移当前变换（等价 cairo `translate`）。
    fn translate(&mut self, x: f64, y: f64);
    /// 缩放当前变换（等价 cairo `scale`）。
    fn scale(&mut self, sx: f64, sy: f64);
    /// 以弧度为单位旋转当前变换（等价 cairo `rotate`）。
    fn rotate(&mut self, radians: f64);

    fn set_color(&mut self, color: Color);
    fn set_line_width(&mut self, width: f64);
    fn set_line_cap(&mut self, cap: Cap);
    fn set_line_join(&mut self, join: Join);
    /// 空切片表示取消虚线（cairo `set_dash(&[], 0.0)`）。
    fn set_dash(&mut self, dashes: &[f64], offset: f64);

    fn fill_path(&mut self, path: &Path2);
    fn stroke_path(&mut self, path: &Path2);

    /// 用纯色铺满当前裁剪区（等价 cairo `set_source_*` + `paint`）。
    fn paint_with(&mut self, color: Color, blend: Blend);
    /// 把目标限制为这块矩形（与已有裁剪求交）。
    fn clip_rect(&mut self, x: f64, y: f64, width: f64, height: f64);

    /// 开始一个离屏组：后续绘制都进组，直到 [`Canvas::pop_group_to_source`]。
    ///
    /// `bounds` 是这次合成的范围（设备像素），后端用它分配最小的中间表面。
    fn push_group(&mut self, bounds: RectF);
    /// 结束离屏组，并把组内容**替换**到 `bounds` 区域（Source 语义）。
    fn pop_group_to_source(&mut self, bounds: RectF);

    /// 把一张表面贴到画布原点上，遵循给定混合模式。
    fn draw_surface(&mut self, surface: &dyn Surface, blend: Blend);
}

/// 矩形（设备像素，f64 便于与视图变换衔接）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl RectF {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn from_bounds(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self::new(x0, y0, x1 - x0, y1 - y0)
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// 向外取整到整数像素，并夹到 `surface_width` × `surface_height` 范围内。
    pub fn to_pixel_rect(self, surface_width: u32, surface_height: u32) -> RectF {
        let x0 = self.x.floor().max(0.0);
        let y0 = self.y.floor().max(0.0);
        let x1 = (self.x + self.width).ceil().min(surface_width as f64);
        let y1 = (self.y + self.height).ceil().min(surface_height as f64);
        RectF::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
    }
}

/// 离屏位图表面（存储为预乘 RGBA，对外读写 straight RGBA）。
pub trait Surface: Any {
    fn width(&self) -> u32;
    fn height(&self) -> u32;

    /// 在表面上绘制。回调拿到的画布坐标就是表面设备像素。
    fn with_canvas(&mut self, f: &mut dyn FnMut(&mut dyn Canvas));

    /// 读出 straight（未预乘）RGBA，长度 = width * height * 4。
    fn read_rgba(&self) -> Vec<u8>;

    /// 写入 straight RGBA（截图中读到的那种字节序）。
    fn write_rgba(&mut self, rgba: &[u8]) -> Result<(), PaintError>;

    /// 用纯色铺满整张表面。
    fn fill(&mut self, color: Color);

    /// 供后端在类型擦除后取回具体类型。
    fn as_any(&self) -> &dyn Any;
}

#[derive(Debug, Clone)]
pub struct PaintError(pub String);

impl fmt::Display for PaintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PaintError {}

/// 按当前平台创建一张离屏表面。
pub fn new_surface(width: u32, height: u32) -> Result<Box<dyn Surface>, PaintError> {
    if width == 0 || height == 0 {
        return Err(PaintError("表面尺寸无效".to_string()));
    }
    Ok(Box::new(PlatformSurface::new(width, height)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_builder_is_empty_until_an_operation_is_pushed() {
        assert!(PathBuilder2::new().finish().is_none());
        let mut builder = PathBuilder2::new();
        builder.move_to(1.0, 2.0);
        assert!(builder.finish().is_some());
    }

    #[test]
    fn rect_is_closed() {
        let mut builder = PathBuilder2::new();
        builder.rect(0.0, 0.0, 10.0, 5.0);
        let path = builder.finish().unwrap();
        assert!(matches!(path.ops().last(), Some(PathOp::Close)));
        // move + 3 次 line + close（起点不重复记录）
        assert_eq!(path.ops().len(), 5);
    }

    #[test]
    fn pixel_rect_expands_outwards_and_clamps() {
        let rect = RectF::new(-3.2, 4.6, 20.0, 30.0).to_pixel_rect(100, 100);
        assert_eq!(rect, RectF::new(0.0, 4.0, 17.0, 31.0));

        let clipped = RectF::new(90.0, 90.0, 50.0, 50.0).to_pixel_rect(100, 100);
        assert_eq!(clipped, RectF::new(90.0, 90.0, 10.0, 10.0));
    }
}
