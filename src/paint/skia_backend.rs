//! Windows 后端：把 [`Canvas`] / [`Surface`] 映射到 tiny-skia。
//!
//! tiny-skia 没有 cairo 那样的图形状态栈，也没有 `push_group` / `clip` / `set_device_scale`，
//! 所以这里自己维护：
//!
//! - **状态栈**：变换、颜色、线宽、端点、连接、虚线、裁剪，`save`/`restore` 压栈；
//! - **裁剪**：把裁剪矩形和当前目标（整张表面或离屏组）求交，必要时建一张裁剪区大小的
//!   临时位图，画完再贴回——裁剪区通常就是本帧脏矩形（几十到几百像素），成本可控；
//! - **离屏组**：`push_group` 按 `bounds` 建 pixmap，组内坐标整体平移 `-bounds.origin`，
//!   `pop_group_to_source` 用 `BlendMode::Source` 贴回，保持「透明区域也覆盖」的语义
//!   （README 第 16 条的残影回归点）。
//!
//! 坐标一律是表面设备像素；像素存储为预乘 RGBA。

use tiny_skia::{
    BlendMode, Color as SkColor, FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, PixmapRef,
    Rect as SkRect, Stroke, StrokeDash, Transform,
};

use crate::paint::{Blend, Canvas, Cap, Color, Join, Path2, PathOp, PaintError, RectF, Surface};

pub struct SkiaCanvas<'a> {
    target: &'a mut Pixmap,
    state: State,
    saved: Vec<State>,
    groups: Vec<Group>,
}

#[derive(Debug, Clone)]
struct State {
    transform: Transform,
    color: Color,
    line_width: f64,
    cap: Cap,
    join: Join,
    dash: Vec<f64>,
    dash_offset: f64,
    /// 设备坐标里的裁剪矩形（与 cairo 的 `clip()` 叠加语义一致）。
    clip: Option<RectF>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            transform: Transform::identity(),
            color: Color::new(0, 0, 0, 255),
            line_width: 2.0,
            cap: Cap::Round,
            join: Join::Round,
            dash: Vec::new(),
            dash_offset: 0.0,
            clip: None,
        }
    }
}

struct Group {
    /// 组在设备坐标里的范围（已取整）。
    bounds: RectF,
    pixmap: Pixmap,
}

impl<'a> SkiaCanvas<'a> {
    pub fn new(target: &'a mut Pixmap) -> Self {
        Self {
            target,
            state: State::default(),
            saved: Vec::new(),
            groups: Vec::new(),
        }
    }

    /// 当前绘制目标的设备坐标范围。
    fn target_rect(&self) -> RectF {
        match self.groups.last() {
            Some(group) => group.bounds,
            None => RectF::new(
                0.0,
                0.0,
                self.target.width() as f64,
                self.target.height() as f64,
            ),
        }
    }

    fn paint(&self) -> Paint<'static> {
        let mut paint = Paint::default();
        paint.set_color(to_sk_color(self.state.color));
        paint.anti_alias = true;
        paint
    }

    fn stroke(&self) -> Stroke {
        Stroke {
            width: self.state.line_width.max(0.0) as f32,
            miter_limit: 4.0,
            line_cap: match self.state.cap {
                Cap::Butt => tiny_skia::LineCap::Butt,
                Cap::Round => tiny_skia::LineCap::Round,
                Cap::Square => tiny_skia::LineCap::Square,
            },
            line_join: match self.state.join {
                Join::Miter => tiny_skia::LineJoin::Miter,
                Join::Round => tiny_skia::LineJoin::Round,
                Join::Bevel => tiny_skia::LineJoin::Bevel,
            },
            dash: if self.state.dash.is_empty() {
                None
            } else {
                StrokeDash::new(
                    self.state.dash.iter().map(|value| *value as f32).collect(),
                    self.state.dash_offset as f32,
                )
            },
        }
    }

    /// 在（裁剪后的）目标区域内执行一次绘制。
    ///
    /// `draw` 收到的是位图与「设备坐标 → 位图坐标」的变换。
    ///
    /// 关键点：**组内**的裁剪是相对组原点的，`target_rect()` 已经是组的设备范围，
    /// 所以这里只需要把设备坐标平移到组内即可；不能再用整张表面的尺寸去比较，
    /// 否则 `clip_rect` 命中一小块时会被误判成「非全量」，导致只画出局部。
    fn draw_clipped(&mut self, blend: Blend, mut draw: impl FnMut(&mut Pixmap, Transform)) {
        let target_rect = self.target_rect();
        let effective = match self.state.clip {
            Some(clip) => intersect(clip, target_rect),
            None => target_rect,
        };
        // 取整到像素：按**设备坐标**夹到目标范围。
        // 注意不能用位图尺寸当上限——组内原点不是 (0,0)，那样会把坐标截到左上角。
        let x0 = effective.x.floor().max(target_rect.x);
        let y0 = effective.y.floor().max(target_rect.y);
        let x1 = (effective.x + effective.width)
            .ceil()
            .min(target_rect.x + target_rect.width);
        let y1 = (effective.y + effective.height)
            .ceil()
            .min(target_rect.y + target_rect.height);
        let bounds = RectF::new(x0, y0, x1 - x0, y1 - y0);
        if bounds.is_empty() {
            return;
        }

        let full = bounds == target_rect;
        // 先在用户坐标系里绘制，再整体平移到目标位图的局部原点。
        // 必须用 post_translate：tiny-skia 的 map_point 是 `p * s + t`，
        // pre_translate 会变成「先平移再缩放」，把整块内容推到左上角。
        let transform = self
            .state
            .transform
            .post_translate(-bounds.x as f32, -bounds.y as f32);

        if full {
            let target = match self.groups.last_mut() {
                Some(group) => &mut group.pixmap,
                None => self.target,
            };
            draw(target, transform);
            return;
        }

        // 裁剪区小于目标：画进临时位图再贴回，避免越界绘制。
        let Some(mut scratch) = Pixmap::new(bounds.width as u32, bounds.height as u32) else {
            return;
        };
        draw(&mut scratch, transform);
        match self.groups.last_mut() {
            Some(group) => {
                let x = (bounds.x - group.bounds.x) as i32;
                let y = (bounds.y - group.bounds.y) as i32;
                blit(&mut group.pixmap, &scratch, x, y, blend);
            }
            None => blit(self.target, &scratch, bounds.x as i32, bounds.y as i32, blend),
        }
    }
}

impl Canvas for SkiaCanvas<'_> {
    fn save(&mut self) {
        self.saved.push(self.state.clone());
    }

    fn restore(&mut self) {
        if let Some(state) = self.saved.pop() {
            self.state = state;
        }
    }

    fn translate(&mut self, x: f64, y: f64) {
        // cairo 的 translate 是在当前用户坐标系里追加变换（左乘 CTM），
        // 对应 tiny-skia 的 pre_translate。
        self.state.transform = self.state.transform.pre_translate(x as f32, y as f32);
    }

    fn scale(&mut self, sx: f64, sy: f64) {
        self.state.transform = self.state.transform.pre_scale(sx as f32, sy as f32);
    }

    fn rotate(&mut self, radians: f64) {
        let degrees = radians.to_degrees() as f32;
        self.state.transform = self.state.transform.pre_rotate(degrees);
    }

    fn set_color(&mut self, color: Color) {
        self.state.color = color;
    }

    fn set_line_width(&mut self, width: f64) {
        self.state.line_width = width;
    }

    fn set_line_cap(&mut self, cap: Cap) {
        self.state.cap = cap;
    }

    fn set_line_join(&mut self, join: Join) {
        self.state.join = join;
    }

    fn set_dash(&mut self, dashes: &[f64], offset: f64) {
        self.state.dash = dashes.to_vec();
        self.state.dash_offset = offset;
    }

    fn fill_path(&mut self, path: &Path2) {
        let Some(skia_path) = build_skia_path(path) else {
            return;
        };
        let paint = self.paint();
        self.draw_clipped(Blend::Over, |target, transform| {
            target.fill_path(&skia_path, &paint, FillRule::Winding, transform, None);
        });
    }

    fn stroke_path(&mut self, path: &Path2) {
        let Some(skia_path) = build_skia_path(path) else {
            return;
        };
        let paint = self.paint();
        let stroke = self.stroke();
        self.draw_clipped(Blend::Over, |target, transform| {
            target.stroke_path(&skia_path, &paint, &stroke, transform, None);
        });
    }

    fn paint_with(&mut self, color: Color, blend: Blend) {
        let color = to_sk_color(color);
        let mode = to_blend(blend);
        self.draw_clipped(blend, |target, _transform| {
            let rect =
                SkRect::from_xywh(0.0, 0.0, target.width() as f32, target.height() as f32)
                    .expect("非空位图");
            let mut paint = Paint::default();
            paint.set_color(color);
            paint.anti_alias = false;
            paint.blend_mode = mode;
            target.fill_rect(rect, &paint, Transform::identity(), None);
        });
    }

    fn clip_rect(&mut self, x: f64, y: f64, width: f64, height: f64) {
        let requested = RectF::new(x, y, width.max(0.0), height.max(0.0));
        self.state.clip = Some(match self.state.clip {
            Some(existing) => intersect(existing, requested),
            None => requested,
        });
    }

    fn push_group(&mut self, bounds: RectF) {
        let bounds = match self.state.clip {
            Some(clip) => intersect(clip, bounds),
            None => bounds,
        };
        let target_rect = self.target_rect();
        let bounds = intersect(bounds, target_rect);
        // 组内坐标从 0 开始，尺寸取整后至少 1 px 才能建位图。
        let bounds = RectF::new(
            bounds.x.floor(),
            bounds.y.floor(),
            bounds.width.ceil().max(1.0),
            bounds.height.ceil().max(1.0),
        );
        let bounds = intersect(bounds, target_rect);
        let Some(pixmap) = Pixmap::new(
            bounds.width.max(1.0) as u32,
            bounds.height.max(1.0) as u32,
        ) else {
            return;
        };
        // 组已经按裁剪范围建好了：进组后清掉 clip，否则组内会被再裁一次，
        // 表现为只画出组的左上角一块。
        self.state.clip = None;
        self.groups.push(Group { bounds, pixmap });
    }

    fn pop_group_to_source(&mut self, _bounds: RectF) {
        let Some(group) = self.groups.pop() else {
            return;
        };
        if group.bounds.is_empty() {
            return;
        }
        match self.groups.last_mut() {
            Some(parent) => blit(
                &mut parent.pixmap,
                &group.pixmap,
                (group.bounds.x - parent.bounds.x) as i32,
                (group.bounds.y - parent.bounds.y) as i32,
                Blend::Source,
            ),
            None => blit(
                self.target,
                &group.pixmap,
                group.bounds.x as i32,
                group.bounds.y as i32,
                Blend::Source,
            ),
        }
    }

    fn draw_surface(&mut self, surface: &dyn Surface, blend: Blend) {
        let Some(skia) = surface.as_any().downcast_ref::<SkiaSurface>() else {
            log::warn!("非 tiny-skia 表面被贴到 tiny-skia 画布上，已忽略");
            return;
        };
        match self.groups.last_mut() {
            Some(group) => {
                // 表面与设备坐标 1:1 对齐，只需要减掉组原点。
                blit(
                    &mut group.pixmap,
                    &skia.pixmap,
                    (-group.bounds.x) as i32,
                    (-group.bounds.y) as i32,
                    blend,
                );
            }
            None => blit(self.target, &skia.pixmap, 0, 0, blend),
        }
    }
}

fn build_skia_path(path: &Path2) -> Option<tiny_skia::Path> {
    let mut builder = PathBuilder::new();
    for op in path.ops() {
        match *op {
            PathOp::MoveTo(x, y) => builder.move_to(x as f32, y as f32),
            PathOp::LineTo(x, y) => builder.line_to(x as f32, y as f32),
            PathOp::Close => builder.close(),
            PathOp::Circle(x, y, radius) => {
                builder.push_circle(x as f32, y as f32, radius.max(0.0) as f32)
            }
        }
    }
    builder.finish()
}

fn blit(dst: &mut Pixmap, src: &Pixmap, x: i32, y: i32, blend: Blend) {
    let paint = PixmapPaint {
        blend_mode: to_blend(blend),
        quality: tiny_skia::FilterQuality::Nearest,
        ..Default::default()
    };
    dst.draw_pixmap(x, y, src.as_ref(), &paint, Transform::identity(), None);
}

fn to_blend(blend: Blend) -> BlendMode {
    match blend {
        Blend::Source => BlendMode::Source,
        Blend::Over => BlendMode::SourceOver,
    }
}

fn to_sk_color(color: Color) -> SkColor {
    SkColor::from_rgba8(color.r, color.g, color.b, color.a)
}

fn intersect(a: RectF, b: RectF) -> RectF {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    RectF::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

pub struct SkiaSurface {
    pixmap: Pixmap,
}

impl SkiaSurface {
    pub fn new(width: u32, height: u32) -> Result<Self, PaintError> {
        Pixmap::new(width, height)
            .map(|pixmap| Self { pixmap })
            .ok_or_else(|| PaintError("创建 tiny-skia 表面失败".to_string()))
    }

    /// 预乘 RGBA 原始字节（tiny-skia 的存储格式，紧密排列、无 stride 填充）。
    ///
    /// Windows 分层窗口要的是预乘 **BGRA**，字节序转换放在 `platform::win` 里做，
    /// 避免这里为了一个平台把格式写死。
    pub fn premultiplied_rgba(&self) -> &[u8] {
        self.pixmap.data()
    }

    pub fn pixmap(&self) -> &Pixmap {
        &self.pixmap
    }

    pub fn pixmap_mut(&mut self) -> &mut Pixmap {
        &mut self.pixmap
    }

    pub fn as_ref(&self) -> PixmapRef<'_> {
        self.pixmap.as_ref()
    }
}

impl Surface for SkiaSurface {
    fn width(&self) -> u32 {
        self.pixmap.width()
    }

    fn height(&self) -> u32 {
        self.pixmap.height()
    }

    fn with_canvas(&mut self, f: &mut dyn FnMut(&mut dyn Canvas)) {
        let mut canvas = SkiaCanvas::new(&mut self.pixmap);
        f(&mut canvas);
    }

    fn read_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity((self.pixmap.width() * self.pixmap.height() * 4) as usize);
        for pixel in self.pixmap.pixels() {
            let color = pixel.demultiply();
            out.extend_from_slice(&[color.red(), color.green(), color.blue(), color.alpha()]);
        }
        out
    }

    fn write_rgba(&mut self, rgba: &[u8]) -> Result<(), PaintError> {
        let width = self.pixmap.width() as usize;
        let height = self.pixmap.height() as usize;
        if rgba.len() < width * height * 4 {
            return Err(PaintError("像素数据长度不足".to_string()));
        }
        let data = self.pixmap.data_mut();
        for (index, pixel) in data.chunks_exact_mut(4).enumerate() {
            let src = &rgba[index * 4..index * 4 + 4];
            let a = src[3] as u32;
            let premultiply = |value: u8| ((value as u32 * a + 127) / 255) as u8;
            pixel[0] = premultiply(src[0]);
            pixel[1] = premultiply(src[1]);
            pixel[2] = premultiply(src[2]);
            pixel[3] = src[3];
        }
        Ok(())
    }

    fn fill(&mut self, color: Color) {
        self.pixmap.fill(to_sk_color(color));
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_blit_overwrites_transparent_pixels() {
        let mut dst = Pixmap::new(8, 8).unwrap();
        dst.fill(SkColor::from_rgba8(0, 0, 255, 255));
        let src = Pixmap::new(4, 4).unwrap(); // 全透明
        blit(&mut dst, &src, 2, 2, Blend::Source);
        assert_eq!(
            dst.pixel(3, 3).map(|pixel| pixel.alpha()),
            Some(0),
            "Source 贴图必须把透明像素也写回去"
        );
    }

    #[test]
    fn group_bounds_are_intersected_with_clip() {
        let mut target = Pixmap::new(100, 100).unwrap();
        let mut canvas = SkiaCanvas::new(&mut target);
        canvas.clip_rect(10.0, 10.0, 20.0, 20.0);
        canvas.push_group(RectF::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(
            canvas.groups.last().map(|group| group.bounds),
            Some(RectF::new(10.0, 10.0, 20.0, 20.0))
        );
    }

    #[test]
    fn restore_returns_to_saved_state() {
        let mut target = Pixmap::new(16, 16).unwrap();
        let mut canvas = SkiaCanvas::new(&mut target);
        canvas.set_line_width(3.0);
        canvas.save();
        canvas.set_line_width(9.0);
        canvas.restore();
        assert_eq!(canvas.state.line_width, 3.0);
    }
}
