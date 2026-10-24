use link_ink::model::{Document, Point};
use link_ink::platform::{MonitorInfo, RectF, RectI};
use link_ink::view::ViewTransform;

fn monitor() -> MonitorInfo {
    MonitorInfo {
        physical: RectI {
            x: 0,
            y: 0,
            width: 4384,
            height: 2466,
        },
        logical: RectF {
            x: 0.0,
            y: 0.0,
            width: 2192.0,
            height: 1233.0,
        },
        scale: 2.0,
    }
}

#[test]
fn page_fills_monitor_at_2x() {
    let monitor = monitor();
    let doc = Document::new(monitor.width_pt(), monitor.height_pt());
    let view = ViewTransform::for_monitor(&monitor, doc.width_pt, doc.height_pt);

    assert!((doc.width_pt - 1644.0).abs() < 1e-9);
    assert!((doc.height_pt - 924.75).abs() < 1e-9);
    assert!((view.pt_to_logical() - 96.0 / 72.0).abs() < 1e-9);

    let (x, y) = view.page_to_physical(Point::new(0.0, 0.0));
    assert!(x.abs() < 1e-6 && y.abs() < 1e-6);
    let (x, y) = view.page_to_physical(Point::new(doc.width_pt, doc.height_pt));
    assert!((x - 4384.0).abs() < 1e-6);
    assert!((y - 2466.0).abs() < 1e-6);

    let (lx, ly) = view.page_to_logical(Point::new(doc.width_pt, doc.height_pt));
    assert!((lx - 2192.0).abs() < 1e-6);
    assert!((ly - 1233.0).abs() < 1e-6);
}

#[test]
fn logical_page_roundtrip() {
    let monitor = monitor();
    let doc = Document::new(monitor.width_pt(), monitor.height_pt());
    let view = ViewTransform::for_monitor(&monitor, doc.width_pt, doc.height_pt);

    for (x, y) in [
        (0.0, 0.0),
        (100.0, 200.0),
        (2192.0, 1233.0),
        (731.25, 555.5),
    ] {
        let page = view.logical_to_page(x, y);
        let (rx, ry) = view.page_to_logical(page);
        assert!((rx - x).abs() < 1e-9);
        assert!((ry - y).abs() < 1e-9);
        let (px, py) = view.logical_to_physical(x, y);
        assert!((px - x * 2.0).abs() < 1e-6);
        assert!((py - y * 2.0).abs() < 1e-6);
    }
}

#[test]
fn smaller_page_is_centered() {
    let monitor = monitor();
    let view = ViewTransform::for_monitor(&monitor, 800.0, 600.0);
    let scale = (2192.0f64 / 800.0).min(1233.0 / 600.0);
    assert!((view.pt_to_logical() - scale).abs() < 1e-9);
    let (ox, oy) = view.origin_logical();
    assert!((ox - (2192.0 - 800.0 * scale) / 2.0).abs() < 1e-9);
    assert!((oy - (1233.0 - 600.0 * scale) / 2.0).abs() < 1e-9);
    let (px, py) = view.page_to_physical(Point::new(800.0, 600.0));
    assert!((px - (ox + 800.0 * scale) * 2.0).abs() < 1e-6);
    assert!((py - (oy + 600.0 * scale) * 2.0).abs() < 1e-6);
}

#[test]
fn non_zero_monitor_origin_offsets_physical_mapping() {
    let monitor = MonitorInfo {
        physical: RectI {
            x: 1920,
            y: 0,
            width: 2560,
            height: 1440,
        },
        logical: RectF {
            x: 1920.0,
            y: 0.0,
            width: 1280.0,
            height: 720.0,
        },
        scale: 2.0,
    };
    let doc = Document::new(monitor.width_pt(), monitor.height_pt());
    let view = ViewTransform::for_monitor(&monitor, doc.width_pt, doc.height_pt);
    let (x, y) = view.page_to_physical(Point::new(0.0, 0.0));
    assert!((x - 1920.0).abs() < 1e-6);
    assert!(y.abs() < 1e-6);
    let (x, y) = view.page_to_physical(Point::new(doc.width_pt, doc.height_pt));
    assert!((x - 4480.0).abs() < 1e-6);
    assert!((y - 1440.0).abs() < 1e-6);
}
