//! Linux 后端：把 [`Canvas`] / [`Surface`] 映射到 cairo。
//!
//! 每个方法都与改造前的原始 cairo 调用一一对应（见改造前的 `src/render.rs`、
//! `src/cache.rs`、`src/export.rs`），变换与组合语义尤其要小心：
//!
//! - `translate` / `scale` / `rotate` 直接改 CTM，与 cairo 一致；
//! - `push_group` / `pop_group_to_source` 用 cairo 原生 group，保持原子合成；
//! - `draw_surface` 用 `Operator::Source` 贴图，透明区域也覆盖目标。

use std::cell::RefCell;
use std::rc::Rc;

use cairo::{Context, Format, ImageSurface, Operator};

use crate::paint::{
    Blend, Canvas, Cap, Color, Join, Path2, PathOp, PaintError, RectF, Surface,
};

pub struct CairoCanvas {
    cr: Context,
    state: CanvasState,
    /// 离屏组栈：每层记录组覆盖的范围，pop 时把内容贴回该区域。
    groups: Vec<RectF>,
}

#[derive(Debug, Clone)]
struct CanvasState {
    color: Color,
    line_width: f64,
    cap: Cap,
    join: Join,
    dash: Vec<f64>,
    dash_offset: f64,
    operator: Operator,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            color: Color::new(0, 0, 0, 255),
            line_width: 2.0,
            cap: Cap::Round,
            join: Join::Round,
            dash: Vec::new(),
            dash_offset: 0.0,
            operator: Operator::Over,
        }
    }
}

impl CairoCanvas {
    pub fn new(cr: Context) -> Self {
        let canvas = Self {
            cr,
            state: CanvasState::default(),
            groups: Vec::new(),
        };
        canvas.apply_state();
        canvas
    }

    /// 让画布坐标成为**设备像素**。
    ///
    /// GDK 给窗口表面设了 `device_scale`，于是 cairo 用户坐标是逻辑像素
    /// （device = scale × user）。上层统一按设备像素绘制，所以这里把 CTM 乘
    /// `1 / device_scale` 抵消掉：调用方给的物理坐标 p 最终落在设备像素 p。
    pub fn with_device_scale(cr: Context, device_scale: f64) -> Self {
        if device_scale.is_finite() && device_scale > 0.0 && (device_scale - 1.0).abs() > 1e-9 {
            let inverse = 1.0 / device_scale;
            cr.scale(inverse, inverse);
        }
        Self::new(cr)
    }

    /// 用 GTK 绘制回调给的上下文借用一层（内部是 cairo 的引用计数句柄）。
    pub fn from_ref(cr: &Context, device_scale: f64) -> Self {
        Self::with_device_scale(cr.clone(), device_scale)
    }

    fn apply_state(&self) {
        let cr = &self.cr;
        cr.set_source_rgba(
            self.state.color.r as f64 / 255.0,
            self.state.color.g as f64 / 255.0,
            self.state.color.b as f64 / 255.0,
            self.state.color.a as f64 / 255.0,
        );
        cr.set_line_width(self.state.line_width);
        cr.set_line_cap(match self.state.cap {
            Cap::Butt => cairo::LineCap::Butt,
            Cap::Round => cairo::LineCap::Round,
            Cap::Square => cairo::LineCap::Square,
        });
        cr.set_line_join(match self.state.join {
            Join::Miter => cairo::LineJoin::Miter,
            Join::Round => cairo::LineJoin::Round,
            Join::Bevel => cairo::LineJoin::Bevel,
        });
        cr.set_operator(self.state.operator);
        cr.set_dash(&self.state.dash, self.state.dash_offset);
    }

    fn build_path(&self, path: &Path2) {
        self.cr.new_path();
        for op in path.ops() {
            match *op {
                PathOp::MoveTo(x, y) => self.cr.move_to(x, y),
                PathOp::LineTo(x, y) => self.cr.line_to(x, y),
                PathOp::Close => self.cr.close_path(),
                PathOp::Circle(x, y, radius) => {
                    // cairo 的 arc 会把当前点连到弧起点，所以先断开子路径。
                    self.cr.new_sub_path();
                    self.cr.arc(x, y, radius, 0.0, std::f64::consts::TAU);
                }
            }
        }
    }
}

impl Canvas for CairoCanvas {
    fn save(&mut self) {
        self.cr.save().ok();
    }

    fn restore(&mut self) {
        self.cr.restore().ok();
    }

    fn translate(&mut self, x: f64, y: f64) {
        self.cr.translate(x, y);
    }

    fn scale(&mut self, sx: f64, sy: f64) {
        self.cr.scale(sx, sy);
    }

    fn rotate(&mut self, radians: f64) {
        self.cr.rotate(radians);
    }

    fn set_color(&mut self, color: Color) {
        self.state.color = color;
        self.cr.set_source_rgba(
            color.r as f64 / 255.0,
            color.g as f64 / 255.0,
            color.b as f64 / 255.0,
            color.a as f64 / 255.0,
        );
    }

    fn set_line_width(&mut self, width: f64) {
        self.state.line_width = width;
        self.cr.set_line_width(width);
    }

    fn set_line_cap(&mut self, cap: Cap) {
        self.state.cap = cap;
        self.cr.set_line_cap(match cap {
            Cap::Butt => cairo::LineCap::Butt,
            Cap::Round => cairo::LineCap::Round,
            Cap::Square => cairo::LineCap::Square,
        });
    }

    fn set_line_join(&mut self, join: Join) {
        self.state.join = join;
        self.cr.set_line_join(match join {
            Join::Miter => cairo::LineJoin::Miter,
            Join::Round => cairo::LineJoin::Round,
            Join::Bevel => cairo::LineJoin::Bevel,
        });
    }

    fn set_dash(&mut self, dashes: &[f64], offset: f64) {
        self.state.dash = dashes.to_vec();
        self.state.dash_offset = offset;
        self.cr.set_dash(dashes, offset);
    }

    fn fill_path(&mut self, path: &Path2) {
        self.apply_state();
        self.build_path(path);
        self.cr.fill().ok();
    }

    fn stroke_path(&mut self, path: &Path2) {
        self.apply_state();
        self.build_path(path);
        self.cr.stroke().ok();
    }

    fn paint_with(&mut self, color: Color, blend: Blend) {
        self.cr.set_operator(match blend {
            Blend::Source => Operator::Source,
            Blend::Over => Operator::Over,
        });
        self.cr.set_source_rgba(
            color.r as f64 / 255.0,
            color.g as f64 / 255.0,
            color.b as f64 / 255.0,
            color.a as f64 / 255.0,
        );
        self.cr.paint().ok();
        self.cr.set_operator(self.state.operator);
    }

    fn clip_rect(&mut self, x: f64, y: f64, width: f64, height: f64) {
        self.cr.rectangle(x, y, width, height);
        self.cr.clip();
    }

    fn push_group(&mut self, bounds: RectF) {
        self.cr.push_group();
        self.groups.push(bounds);
    }

    fn pop_group_to_source(&mut self, bounds: RectF) {
        let _ = bounds;
        if self.groups.pop().is_some() && self.cr.pop_group_to_source().is_ok() {
            // 组里没有笔迹的地方是透明的，必须用 Source 覆盖回去，
            // 否则上一帧的图形会留在屏幕上（README 第 16 条）。
            let operator = self.state.operator;
            self.cr.set_operator(Operator::Source);
            self.cr.paint().ok();
            self.cr.set_operator(operator);
        }
    }

    fn draw_surface(&mut self, surface: &dyn Surface, blend: Blend) {
        let Some(cairo) = surface.as_any().downcast_ref::<CairoSurface>() else {
            log::warn!("非 cairo 表面被贴到 cairo 画布上，已忽略");
            return;
        };
        let operator = self.state.operator;
        self.cr.set_operator(match blend {
            Blend::Source => Operator::Source,
            Blend::Over => Operator::Over,
        });
        let target = cairo.surface.borrow();
        self.cr.set_source_surface(&*target, 0.0, 0.0).ok();
        self.cr.paint().ok();
        drop(target);
        self.cr.set_operator(operator);
    }
}

pub struct CairoSurface {
    surface: Rc<RefCell<ImageSurface>>,
}

impl CairoSurface {
    pub fn new(width: u32, height: u32) -> Result<Self, PaintError> {
        let surface = ImageSurface::create(Format::ARgb32, width as i32, height as i32)
            .map_err(|error| PaintError(format!("创建 cairo 表面失败：{error}")))?;
        Ok(Self {
            surface: Rc::new(RefCell::new(surface)),
        })
    }
}

impl Surface for CairoSurface {
    fn width(&self) -> u32 {
        self.surface.borrow().width() as u32
    }

    fn height(&self) -> u32 {
        self.surface.borrow().height() as u32
    }

    fn with_canvas(&mut self, f: &mut dyn FnMut(&mut dyn Canvas)) {
        let surface = self.surface.borrow();
        let Ok(context) = Context::new(&*surface) else {
            return;
        };
        let mut canvas = CairoCanvas::new(context);
        f(&mut canvas);
    }

    fn read_rgba(&self) -> Vec<u8> {
        let mut surface = self.surface.borrow_mut();
        surface.flush();
        let width = surface.width() as usize;
        let height = surface.height() as usize;
        let stride = surface.stride() as usize;
        let mut out = vec![0u8; width * height * 4];
        let Ok(data) = surface.data() else {
            return out;
        };
        for row in 0..height {
            let source = &data[row * stride..row * stride + width * 4];
            let target = &mut out[row * width * 4..(row + 1) * width * 4];
            for (dst, pixel) in target.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
                // ARGB32 小端：内存序 B,G,R,A，且是预乘的。
                dst.copy_from_slice(&demultiply(pixel[2], pixel[1], pixel[0], pixel[3]));
            }
        }
        out
    }

    fn write_rgba(&mut self, rgba: &[u8]) -> Result<(), PaintError> {
        let mut surface = self.surface.borrow_mut();
        let width = surface.width() as usize;
        let height = surface.height() as usize;
        let stride = surface.stride() as usize;
        if rgba.len() < width * height * 4 {
            return Err(PaintError("像素数据长度不足".to_string()));
        }
        {
            let mut data = surface
                .data()
                .map_err(|_| PaintError("读取表面数据失败".to_string()))?;
            for (row_index, row) in data.chunks_exact_mut(stride).take(height).enumerate() {
                let source = &rgba[row_index * width * 4..(row_index + 1) * width * 4];
                for (pixel, src) in row[..width * 4]
                    .chunks_exact_mut(4)
                    .zip(source.chunks_exact(4))
                {
                    let (r, g, b) = premultiply(src[0], src[1], src[2], src[3]);
                    pixel[0] = b;
                    pixel[1] = g;
                    pixel[2] = r;
                    pixel[3] = src[3];
                }
            }
        }
        surface.mark_dirty();
        Ok(())
    }

    fn fill(&mut self, color: Color) {
        let surface = self.surface.borrow();
        let Ok(context) = Context::new(&*surface) else {
            return;
        };
        let mut canvas = CairoCanvas::new(context);
        canvas.paint_with(color, Blend::Source);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn demultiply(r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
    if a == 0 {
        [0, 0, 0, 0]
    } else if a == 255 {
        [r, g, b, 255]
    } else {
        let alpha = a as u32;
        [
            ((r as u32 * 255 + alpha / 2) / alpha).min(255) as u8,
            ((g as u32 * 255 + alpha / 2) / alpha).min(255) as u8,
            ((b as u32 * 255 + alpha / 2) / alpha).min(255) as u8,
            a,
        ]
    }
}

fn premultiply(r: u8, g: u8, b: u8, a: u8) -> (u8, u8, u8) {
    if a == 255 {
        return (r, g, b);
    }
    let alpha = a as u32;
    (
        ((r as u32 * alpha + 127) / 255) as u8,
        ((g as u32 * alpha + 127) / 255) as u8,
        ((b as u32 * alpha + 127) / 255) as u8,
    )
}
