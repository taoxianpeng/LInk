use link_ink::export::{self, ExportBackground, ExportedImage, ImageFormat};
use link_ink::model::{Document, Point, Rgba, Stroke};
use link_ink::platform::{CaptureImage, MonitorInfo, RectF, RectI};

fn monitor(physical: (u32, u32), logical: (f64, f64), scale: f64) -> MonitorInfo {
    MonitorInfo {
        physical: RectI {
            x: 0,
            y: 0,
            width: physical.0,
            height: physical.1,
        },
        logical: RectF {
            x: 0.0,
            y: 0.0,
            width: logical.0,
            height: logical.1,
        },
        scale,
    }
}

fn pixel(image: &ExportedImage, x: u32, y: u32) -> [u8; 4] {
    let index = ((y * image.width + x) * 4) as usize;
    [
        image.rgba[index],
        image.rgba[index + 1],
        image.rgba[index + 2],
        image.rgba[index + 3],
    ]
}

fn stroke_document(width_pt: f64, height_pt: f64, from: Point, to: Point, width: f64) -> Document {
    let mut document = Document::new(width_pt, height_pt);
    document
        .active_layer_mut()
        .strokes
        .push(Stroke::constant(Rgba::BLACK, width, vec![from, to]));
    document
}

#[test]
fn blank_background_is_white_and_stroke_is_composited_at_physical_scale() {
    let monitor = monitor((200, 100), (200.0, 100.0), 1.0);
    let document = stroke_document(
        150.0,
        75.0,
        Point::new(20.0, 37.5),
        Point::new(130.0, 37.5),
        5.0,
    );

    let image = export::render(&document, &monitor, ExportBackground::Blank).unwrap();

    assert_eq!((image.width, image.height), (200, 100));
    assert_eq!(image.rgba.len(), 200 * 100 * 4);
    assert_eq!(pixel(&image, 5, 5), [255, 255, 255, 255]);
    let center = pixel(&image, 100, 50);
    assert_eq!(center[3], 255, "导出图像必须不透明");
    assert!(center[0] < 40, "中心像素应接近黑色，实际 {center:?}");
}

#[test]
fn empty_document_on_blank_background_is_pure_white() {
    let monitor = monitor((32, 16), (32.0, 16.0), 1.0);
    let document = Document::new(24.0, 12.0);
    let image = export::render(&document, &monitor, ExportBackground::Blank).unwrap();
    assert!(image
        .rgba
        .chunks_exact(4)
        .all(|pixel| pixel == [255, 255, 255, 255]));
}

#[test]
fn screen_background_preserves_captured_pixels() {
    let monitor = monitor((4, 2), (4.0, 2.0), 1.0);
    let pixels: Vec<[u8; 4]> = (0..8)
        .map(|index| [index as u8 * 20, 40, 200 - index as u8 * 10, 255])
        .collect();
    let rgba = pixels.iter().flatten().copied().collect();
    let capture = CaptureImage {
        width: 4,
        height: 2,
        origin_px: (0, 0),
        rgba,
    };
    let document = Document::new(3.0, 1.5);

    let image = export::render(&document, &monitor, ExportBackground::Screen(&capture)).unwrap();

    for (index, expected) in pixels.iter().enumerate() {
        let x = index as u32 % 4;
        let y = index as u32 / 4;
        assert_eq!(pixel(&image, x, y), *expected, "像素 {index}");
    }
}

#[test]
fn hidpi_monitor_maps_page_points_to_physical_pixels() {
    let monitor = monitor((200, 100), (100.0, 50.0), 2.0);
    let document = stroke_document(
        75.0,
        37.5,
        Point::new(0.0, 18.75),
        Point::new(75.0, 18.75),
        1.0,
    );

    let image = export::render(&document, &monitor, ExportBackground::Blank).unwrap();

    assert_eq!((image.width, image.height), (200, 100));
    let expected = pixel(&image, 100, 50);
    assert!(expected[0] < 40, "物理 y=50 应有笔迹，实际 {expected:?}");
    assert_eq!(
        pixel(&image, 100, 25),
        [255, 255, 255, 255],
        "逻辑 y 坐标未按 2x 缩放"
    );
}

#[test]
fn png_and_jpeg_encoders_emit_decodable_images() {
    let monitor = monitor((16, 16), (16.0, 16.0), 1.0);
    let document = stroke_document(12.0, 12.0, Point::new(2.0, 6.0), Point::new(10.0, 6.0), 2.0);
    let image = export::render(&document, &monitor, ExportBackground::Blank).unwrap();

    let png = export::encode(&image, ImageFormat::Png).unwrap();
    assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G']);
    let decoded = image::load_from_memory(&png).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (16, 16));

    let jpeg = export::encode(&image, ImageFormat::Jpeg).unwrap();
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    let decoded = image::load_from_memory(&jpeg).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (16, 16));
}

#[test]
fn mismatched_capture_size_is_rejected() {
    let monitor = monitor((8, 8), (8.0, 8.0), 1.0);
    let capture = CaptureImage {
        width: 8,
        height: 4,
        origin_px: (0, 0),
        rgba: vec![255; 8 * 4 * 4],
    };
    let document = Document::new(6.0, 6.0);
    let error =
        export::render(&document, &monitor, ExportBackground::Screen(&capture)).unwrap_err();
    assert!(error.to_string().contains("不一致"), "{error}");
}
