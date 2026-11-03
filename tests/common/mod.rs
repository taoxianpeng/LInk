//! 后端无关的测试辅助：建立离屏表面、读取像素。
//!
//! Linux 上走 cairo，Windows 上走 tiny-skia；测试只关心「这个物理像素有没有颜色」，
//! 不关心具体光栅器，因此也允许两边抗锯齿差异（断言用阈值而不是逐字节）。

#![allow(dead_code)]

use link_ink::paint::{new_surface, Canvas, Color, RectF, Surface};

/// 与窗口同尺寸的表面（设备像素 = 逻辑像素，测试里不做缩放）。
pub struct TestTarget {
    surface: Box<dyn Surface>,
}

impl TestTarget {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            surface: new_surface(width, height).expect("创建测试表面"),
        }
    }

    pub fn canvas(&mut self, f: impl FnOnce(&mut dyn Canvas)) {
        let mut f = Some(f);
        self.surface.with_canvas(&mut |canvas| {
            if let Some(f) = f.take() {
                f(canvas);
            }
        });
    }

    pub fn rgba(&self) -> Vec<u8> {
        self.surface.read_rgba()
    }

    pub fn alpha_at(&self, x: u32, y: u32) -> u8 {
        let width = self.surface.width();
        let rgba = self.surface.read_rgba();
        rgba[((y * width + x) * 4 + 3) as usize]
    }

    pub fn pixel_at(&self, x: u32, y: u32) -> [u8; 4] {
        let width = self.surface.width();
        let rgba = self.surface.read_rgba();
        let index = ((y * width + x) * 4) as usize;
        [
            rgba[index],
            rgba[index + 1],
            rgba[index + 2],
            rgba[index + 3],
        ]
    }

    /// 用 Source 覆盖清成透明（等价旧测试里的 `set_operator(Source) + paint`）。
    pub fn clear(&mut self) {
        self.canvas(|canvas| {
            canvas.paint_with(Color::TRANSPARENT, link_ink::paint::Blend::Source);
        });
    }
}

/// 整张表面的裁剪矩形。
pub fn full_clip(width: f64, height: f64) -> RectF {
    RectF::new(0.0, 0.0, width, height)
}
