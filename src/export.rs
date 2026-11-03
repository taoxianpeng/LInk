use std::fs;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};

use crate::model::Document;
use crate::paint::{Blend, Color, PaintError};
use crate::platform::{CaptureImage, MonitorInfo};
use crate::render;
use crate::view::ViewTransform;

pub const JPEG_QUALITY: u8 = 92;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ImageFormat::Png => "PNG",
            ImageFormat::Jpeg => "JPEG",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ExportBackground<'a> {
    Blank,
    Screen(&'a CaptureImage),
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("导出尺寸无效")]
    InvalidSize,
    #[error(
        "截图尺寸 {actual_width}x{actual_height} 与主屏 {expected_width}x{expected_height} 不一致"
    )]
    BackgroundSize {
        expected_width: u32,
        expected_height: u32,
        actual_width: u32,
        actual_height: u32,
    },
    #[error("创建或绘制导出表面失败：{0}")]
    Paint(#[from] PaintError),
    #[error("读取导出表面失败")]
    SurfaceRead,
    #[error("图片编码失败：{0}")]
    Encode(String),
    #[error("写入图片失败：{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn render(
    document: &Document,
    monitor: &MonitorInfo,
    background: ExportBackground<'_>,
) -> Result<ExportedImage, ExportError> {
    let width = monitor.physical.width;
    let height = monitor.physical.height;
    if width == 0 || height == 0 {
        return Err(ExportError::InvalidSize);
    }

    let mut surface = crate::paint::new_surface(width, height)?;
    match background {
        ExportBackground::Blank => surface.fill(Color::WHITE),
        ExportBackground::Screen(capture) => {
            if capture.width != width || capture.height != height {
                return Err(ExportError::BackgroundSize {
                    expected_width: width,
                    expected_height: height,
                    actual_width: capture.width,
                    actual_height: capture.height,
                });
            }
            surface.write_rgba(&capture.rgba)?;
        }
    }

    let view = ViewTransform::for_monitor(monitor, document.width_pt, document.height_pt)
        .to_physical_pixels(monitor.scale);
    surface.with_canvas(&mut |canvas| {
        canvas.clip_rect(0.0, 0.0, width as f64, height as f64);
        render::draw_document(canvas, document, &view);
    });

    let rgba = surface.read_rgba();
    Ok(ExportedImage {
        width,
        height,
        rgba,
    })
}

/// 渲染某一图层的缩略图（白底）并编码为 PNG。面板用它显示图层预览。
pub fn layer_thumbnail_png(
    layer: &crate::model::Layer,
    page_width_pt: f64,
    page_height_pt: f64,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, ExportError> {
    if width == 0 || height == 0 {
        return Err(ExportError::InvalidSize);
    }
    let mut surface = crate::paint::new_surface(width, height)?;
    surface.with_canvas(&mut |canvas| {
        canvas.paint_with(Color::WHITE, Blend::Source);
        render::draw_layer_thumbnail(
            canvas,
            layer,
            page_width_pt,
            page_height_pt,
            width as f64,
            height as f64,
        );
    });
    let rgba = surface.read_rgba();
    encode(
        &ExportedImage {
            width,
            height,
            rgba,
        },
        ImageFormat::Png,
    )
}

pub fn save(path: &Path, format: ImageFormat, image: &ExportedImage) -> Result<(), ExportError> {
    fs::write(path, encode(image, format)?)?;
    Ok(())
}

pub fn encode(image: &ExportedImage, format: ImageFormat) -> Result<Vec<u8>, ExportError> {
    if image.width == 0 || image.height == 0 {
        return Err(ExportError::InvalidSize);
    }
    let expected = image.width as usize * image.height as usize * 4;
    if image.rgba.len() != expected {
        return Err(ExportError::Encode(format!(
            "像素数据长度 {} 与尺寸不匹配（期望 {expected}）",
            image.rgba.len()
        )));
    }

    let mut out = Vec::new();
    match format {
        ImageFormat::Png => {
            PngEncoder::new(&mut out)
                .write_image(
                    &image.rgba,
                    image.width,
                    image.height,
                    ExtendedColorType::Rgba8,
                )
                .map_err(|error| ExportError::Encode(error.to_string()))?;
        }
        ImageFormat::Jpeg => {
            let rgb = rgb_from_rgba(&image.rgba);
            JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
                .write_image(&rgb, image.width, image.height, ExtendedColorType::Rgb8)
                .map_err(|error| ExportError::Encode(error.to_string()))?;
        }
    }
    Ok(out)
}

fn rgb_from_rgba(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for pixel in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
    }
    rgb
}
