use std::fs;
use std::path::Path;

use cairo::{Context, Format, ImageSurface};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};

use crate::model::Document;
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
    Cairo(#[from] cairo::Error),
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

    let mut surface = ImageSurface::create(Format::ARgb32, width as i32, height as i32)?;
    match background {
        ExportBackground::Blank => fill_solid(&mut surface, [255, 255, 255, 255])?,
        ExportBackground::Screen(capture) => {
            if capture.width != width || capture.height != height {
                return Err(ExportError::BackgroundSize {
                    expected_width: width,
                    expected_height: height,
                    actual_width: capture.width,
                    actual_height: capture.height,
                });
            }
            fill_from_rgba(&mut surface, &capture.rgba)?;
        }
    }

    let view = ViewTransform::for_monitor(monitor, document.width_pt, document.height_pt)
        .to_physical_pixels(monitor.scale);
    {
        let context = Context::new(&surface)?;
        render::draw_document(&context, document, &view)?;
    }
    surface.flush();

    let rgba = read_rgba(&mut surface)?;
    Ok(ExportedImage {
        width,
        height,
        rgba,
    })
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

fn fill_solid(surface: &mut ImageSurface, rgba: [u8; 4]) -> Result<(), ExportError> {
    let width = surface.width() as usize;
    let stride = surface.stride() as usize;
    let mut data = surface.data().map_err(|_| ExportError::SurfaceRead)?;
    for row in data.chunks_exact_mut(stride) {
        for pixel in row[..width * 4].chunks_exact_mut(4) {
            write_pixel(pixel, rgba);
        }
    }
    Ok(())
}

fn fill_from_rgba(surface: &mut ImageSurface, rgba: &[u8]) -> Result<(), ExportError> {
    let width = surface.width() as usize;
    let height = surface.height() as usize;
    let stride = surface.stride() as usize;
    if rgba.len() < width * height * 4 {
        return Err(ExportError::SurfaceRead);
    }
    let mut data = surface.data().map_err(|_| ExportError::SurfaceRead)?;
    for (row_index, row) in data.chunks_exact_mut(stride).take(height).enumerate() {
        let source = &rgba[row_index * width * 4..(row_index + 1) * width * 4];
        for (pixel, src) in row[..width * 4]
            .chunks_exact_mut(4)
            .zip(source.chunks_exact(4))
        {
            write_pixel(pixel, [src[0], src[1], src[2], src[3]]);
        }
    }
    Ok(())
}

fn read_rgba(surface: &mut ImageSurface) -> Result<Vec<u8>, ExportError> {
    let width = surface.width() as usize;
    let height = surface.height() as usize;
    let stride = surface.stride() as usize;
    let data = surface.data().map_err(|_| ExportError::SurfaceRead)?;
    let mut rgba = vec![0u8; width * height * 4];
    for (row_index, row) in data.chunks_exact(stride).take(height).enumerate() {
        let target = &mut rgba[row_index * width * 4..(row_index + 1) * width * 4];
        for (dst, pixel) in target
            .chunks_exact_mut(4)
            .zip(row[..width * 4].chunks_exact(4))
        {
            let (r, g, b, a) = read_pixel(pixel);
            if a == 0 {
                dst.copy_from_slice(&[0, 0, 0, 0]);
            } else if a == 255 {
                dst.copy_from_slice(&[r, g, b, 255]);
            } else {
                let alpha = a as u32;
                dst[0] = ((r as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                dst[1] = ((g as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                dst[2] = ((b as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                dst[3] = a;
            }
        }
    }
    Ok(rgba)
}

fn rgb_from_rgba(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for pixel in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
    }
    rgb
}

#[cfg(target_endian = "little")]
fn write_pixel(pixel: &mut [u8], [r, g, b, a]: [u8; 4]) {
    pixel[0] = b;
    pixel[1] = g;
    pixel[2] = r;
    pixel[3] = a;
}

#[cfg(target_endian = "big")]
fn write_pixel(pixel: &mut [u8], [r, g, b, a]: [u8; 4]) {
    pixel[0] = a;
    pixel[1] = r;
    pixel[2] = g;
    pixel[3] = b;
}

#[cfg(target_endian = "little")]
fn read_pixel(pixel: &[u8]) -> (u8, u8, u8, u8) {
    (pixel[2], pixel[1], pixel[0], pixel[3])
}

#[cfg(target_endian = "big")]
fn read_pixel(pixel: &[u8]) -> (u8, u8, u8, u8) {
    (pixel[1], pixel[2], pixel[3], pixel[0])
}
