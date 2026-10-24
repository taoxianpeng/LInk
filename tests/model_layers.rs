use link_ink::model::{Document, Point, Rgba, Stroke};
use link_ink::xopp;

#[test]
fn layer_visibility_can_be_toggled_and_rejects_no_ops() {
    let mut document = Document::new(100.0, 100.0);
    assert!(document.layers[0].visible, "新图层默认可见");

    assert!(document.toggle_layer_visible(0));
    assert!(!document.layers[0].visible);

    assert!(
        !document.set_layer_visible(0, false),
        "已经是隐藏状态，不应报告变化"
    );

    assert!(document.toggle_layer_visible(0));
    assert!(document.layers[0].visible);

    assert!(!document.toggle_layer_visible(9), "越界索引不应 panic");
    assert!(!document.set_layer_visible(9, false));
}

#[test]
fn rename_layer_trims_and_rejects_empty_names() {
    let mut document = Document::new(100.0, 100.0);
    assert!(document.rename_layer(0, "  草稿  "));
    assert_eq!(document.layers[0].name, "草稿");

    assert!(!document.rename_layer(0, "   "), "空名字必须被拒绝");
    assert_eq!(document.layers[0].name, "草稿");

    assert!(!document.rename_layer(0, "草稿"), "名字未变化时不报告改动");
    assert!(!document.rename_layer(5, "越界"));
}

#[test]
fn hidden_layer_keeps_strokes_and_visibility_is_not_persisted() {
    let mut document = Document::new(75.0, 37.5);
    document.active_layer_mut().strokes.push(Stroke::constant(
        Rgba::BLACK,
        2.0,
        vec![Point::new(1.0, 1.0), Point::new(5.0, 5.0)],
    ));
    document.set_layer_visible(0, false);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("visibility.xopp");
    xopp::save(&path, &document).unwrap();
    let outcome = xopp::load(&path).unwrap();

    assert_eq!(
        outcome.document.layers[0].strokes.len(),
        1,
        "隐藏图层的笔迹必须原样保留"
    );
    assert!(
        outcome.document.layers[0].visible,
        "xopp 没有可见性属性，加载后应恢复为可见"
    );
}

#[test]
fn drawing_on_hidden_layer_makes_it_visible() {
    use link_ink::app::App;
    use link_ink::input::{InputEvent, PointerSample, ToolKind};
    use link_ink::platform::{MonitorInfo, RectF, RectI};
    use link_ink::view::ViewTransform;

    let monitor = MonitorInfo {
        physical: RectI {
            x: 0,
            y: 0,
            width: 1000,
            height: 1000,
        },
        logical: RectF {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 1000.0,
        },
        scale: 1.0,
    };
    let document = Document::new(monitor.width_pt(), monitor.height_pt());
    let view = ViewTransform::for_monitor(&monitor, document.width_pt, document.height_pt);
    let mut app = App::new(document);
    app.document.set_layer_visible(0, false);

    let sample = PointerSample {
        logical: (100.0, 100.0),
        pressure: 0.5,
        pressure_available: true,
        device_has_pressure: true,
        tool: ToolKind::Pen,
        device_name: "测试笔".to_string(),
        tilt: None,
        shift: false,
    };
    app.handle_input(InputEvent::Down(sample.clone()), &view);
    assert!(app.document.layers[0].visible, "落笔应自动显示隐藏图层");

    app.handle_input(InputEvent::Up(sample), &view);
    assert_eq!(app.document.active_layer().strokes.len(), 1);
}
