use link_ink::app::{App, Tool};
use link_ink::input::{InputEvent, PointerSample, ToolKind};
use link_ink::model::{Document, Point};
use link_ink::platform::{MonitorInfo, RectF, RectI};
use link_ink::view::ViewTransform;

fn view_and_document() -> (ViewTransform, Document) {
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
    (view, document)
}

fn mouse(x: f64, y: f64) -> PointerSample {
    PointerSample {
        logical: (x, y),
        pressure: 1.0,
        pressure_available: false,
        device_has_pressure: false,
        tool: ToolKind::Mouse,
        device_name: "测试鼠标".to_string(),
        tilt: None,
        shift: false,
    }
}

fn pen(x: f64, y: f64, pressure: f64) -> PointerSample {
    PointerSample {
        logical: (x, y),
        pressure,
        pressure_available: true,
        device_has_pressure: true,
        tool: ToolKind::Pen,
        device_name: "测试笔".to_string(),
        tilt: None,
        shift: false,
    }
}

fn mouse_shift(x: f64, y: f64) -> PointerSample {
    let mut sample = mouse(x, y);
    sample.shift = true;
    sample
}

#[test]
fn click_commits_a_dot() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    assert!(app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view));
    assert!(app.handle_input(InputEvent::Up(mouse(100.0, 100.0)), &view));

    let strokes = &app.document.active_layer().strokes;
    assert_eq!(strokes.len(), 1);
    assert_eq!(strokes[0].points.len(), 2);
    assert_eq!(strokes[0].points[0], strokes[0].points[1]);
    assert!(strokes[0].widths_pt.is_none());
    assert!(app.is_dirty());
    assert!(!app.is_drawing());
}

#[test]
fn drag_commits_sampled_points() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Move(mouse(200.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse(300.0, 100.0)), &view);

    let stroke = &app.document.active_layer().strokes[0];
    assert_eq!(stroke.points.len(), 3);
    assert!((stroke.points[1].x - 150.0).abs() < 1e-9);
    assert!((stroke.points[2].x - 225.0).abs() < 1e-9);
}

#[test]
fn tiny_movements_are_filtered() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Move(mouse(100.3, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse(100.0, 100.0)), &view);

    let stroke = &app.document.active_layer().strokes[0];
    assert_eq!(stroke.points.len(), 2);
}

#[test]
fn pen_records_per_point_widths() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);
    app.settings.width_pt = 4.0;
    app.settings.pressure_gamma = 1.0;
    app.settings.min_pressure_ratio = 0.25;

    app.handle_input(InputEvent::Down(pen(100.0, 100.0, 0.2)), &view);
    app.handle_input(InputEvent::Move(pen(200.0, 100.0, 1.0)), &view);
    app.handle_input(InputEvent::Up(pen(200.0, 100.0, 1.0)), &view);

    let stroke = &app.document.active_layer().strokes[0];
    let widths = stroke.widths_pt.as_ref().expect("压感笔应保存变宽");
    assert_eq!(widths.len(), stroke.points.len());
    assert!((widths[0] - 4.0 * (0.25 + 0.75 * 0.2)).abs() < 1e-9);
    assert!((widths[1] - 4.0).abs() < 1e-9);
}

#[test]
fn proximity_out_commits_and_cancel_discards() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::ProximityOut, &view);
    assert_eq!(app.document.active_layer().strokes.len(), 1);

    app.handle_input(InputEvent::Down(mouse(300.0, 300.0)), &view);
    app.handle_input(InputEvent::Cancel, &view);
    assert_eq!(app.document.active_layer().strokes.len(), 1);
    assert!(!app.is_drawing());
}

#[test]
fn eraser_removes_whole_stroke() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Move(mouse(300.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse(300.0, 100.0)), &view);
    assert_eq!(app.document.active_layer().strokes.len(), 1);

    app.settings.tool = Tool::Eraser;
    app.handle_input(InputEvent::Down(mouse(200.0, 200.0)), &view);
    app.handle_input(InputEvent::Up(mouse(200.0, 200.0)), &view);
    assert_eq!(app.document.active_layer().strokes.len(), 1);

    app.handle_input(InputEvent::Down(mouse(200.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse(200.0, 100.0)), &view);
    assert!(app.document.active_layer().strokes.is_empty());
}

#[test]
fn fast_erase_segment_crosses_strokes() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse(500.0, 100.0)), &view);
    assert_eq!(app.document.active_layer().strokes.len(), 1);

    app.settings.tool = Tool::Eraser;
    app.handle_input(InputEvent::Down(mouse(200.0, 300.0)), &view);
    app.handle_input(InputEvent::Move(mouse(200.0, 0.0)), &view);
    app.handle_input(InputEvent::Up(mouse(200.0, 0.0)), &view);
    assert!(app.document.active_layer().strokes.is_empty());
}

#[test]
fn eraser_tip_forces_erase_for_pen_tool() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    app.handle_input(InputEvent::Down(pen(100.0, 100.0, 0.8)), &view);
    app.handle_input(InputEvent::Up(pen(100.0, 100.0, 0.8)), &view);
    assert_eq!(app.document.active_layer().strokes.len(), 1);

    let mut eraser_tip = pen(100.0, 100.0, 0.5);
    eraser_tip.tool = ToolKind::Eraser;
    app.handle_input(InputEvent::Down(eraser_tip), &view);
    assert!(app.document.active_layer().strokes.is_empty());
    assert_eq!(app.settings.tool, Tool::Pen);
}

#[test]
fn eraser_only_touches_active_layer() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse(300.0, 100.0)), &view);

    app.document.add_layer_above_active();
    app.handle_input(InputEvent::Down(mouse(100.0, 500.0)), &view);
    app.handle_input(InputEvent::Up(mouse(300.0, 500.0)), &view);
    assert_eq!(app.document.layers[0].strokes.len(), 1);
    assert_eq!(app.document.layers[1].strokes.len(), 1);

    app.settings.tool = Tool::Eraser;
    app.document.active = 0;
    app.handle_input(InputEvent::Down(mouse(200.0, 500.0)), &view);
    app.handle_input(InputEvent::Up(mouse(200.0, 500.0)), &view);
    assert_eq!(app.document.layers[0].strokes.len(), 1);
    assert_eq!(app.document.layers[1].strokes.len(), 1);

    app.document.active = 1;
    app.handle_input(InputEvent::Down(mouse(200.0, 500.0)), &view);
    app.handle_input(InputEvent::Up(mouse(200.0, 500.0)), &view);
    assert_eq!(app.document.layers[0].strokes.len(), 1);
    assert!(app.document.layers[1].strokes.is_empty());
}

#[test]
fn draft_matches_committed_geometry() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Move(mouse(200.0, 100.0)), &view);
    let draft = app.draft_stroke().expect("绘制中应有预览");
    app.handle_input(InputEvent::Up(mouse(200.0, 100.0)), &view);
    let committed = &app.document.active_layer().strokes[0];
    assert_eq!(draft.points, committed.points);
    assert_eq!(draft.widths_pt, committed.widths_pt);
}

#[test]
fn clear_active_layer_marks_dirty() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);
    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse(100.0, 100.0)), &view);

    assert!(app.clear_active_layer());
    assert!(app.document.active_layer().strokes.is_empty());
    assert!(app.is_dirty());
    assert!(!app.clear_active_layer());
}

#[test]
fn stroke_hit_test_in_page_coordinates() {
    let stroke = link_ink::model::Stroke::constant(
        link_ink::model::Rgba::BLACK,
        10.0,
        vec![Point::new(10.0, 10.0), Point::new(110.0, 10.0)],
    );
    assert!(link_ink::model::stroke_hits_segment(
        &stroke,
        Point::new(60.0, 14.0),
        Point::new(60.0, 14.0),
        2.0
    ));
    assert!(!link_ink::model::stroke_hits_segment(
        &stroke,
        Point::new(60.0, 40.0),
        Point::new(60.0, 40.0),
        2.0
    ));
}

#[test]
fn line_shape_commits_two_constant_width_points() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);
    app.settings.tool = Tool::Line;
    app.settings.width_pt = 3.0;

    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    assert!(app.draft_stroke().is_none(), "起点处图形尚无尺寸");
    app.handle_input(InputEvent::Move(mouse(300.0, 200.0)), &view);
    let preview = app.draft_stroke().expect("拖动中应显示直线预览");
    assert_eq!(preview.points.len(), 2);
    assert!((preview.points[0].x - 75.0).abs() < 1e-9);
    assert!((preview.points[1].x - 225.0).abs() < 1e-9);
    assert!((preview.points[1].y - 150.0).abs() < 1e-9);

    app.handle_input(InputEvent::Up(mouse(300.0, 200.0)), &view);
    let stroke = &app.document.active_layer().strokes[0];
    assert_eq!(stroke.points, preview.points);
    assert!(stroke.widths_pt.is_none());
    assert!((stroke.base_width_pt - 3.0).abs() < 1e-9);
    assert!(app.is_dirty());
}

#[test]
fn rect_shape_is_closed_and_shift_makes_square() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);
    app.settings.tool = Tool::Rect;

    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse(300.0, 200.0)), &view);
    let rect = &app.document.active_layer().strokes[0];
    assert_eq!(rect.points.len(), 5);
    assert_eq!(rect.points[0], rect.points[4]);
    assert_eq!(rect.points[0], Point::new(75.0, 75.0));
    assert_eq!(rect.points[2], Point::new(225.0, 150.0));

    app.document.add_layer_above_active();
    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse_shift(400.0, 200.0)), &view);
    let square = &app.document.active_layer().strokes[0];
    let width = (square.points[1].x - square.points[0].x).abs();
    let height = (square.points[2].y - square.points[1].y).abs();
    assert!((width - height).abs() < 1e-9);
    assert!((width - 75.0).abs() < 1e-9);
}

#[test]
fn ellipse_shape_samples_closed_curve_and_shift_makes_circle() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);
    app.settings.tool = Tool::Ellipse;

    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse(400.0, 200.0)), &view);
    let ellipse = &app.document.active_layer().strokes[0];
    assert_eq!(ellipse.points.len(), 97);
    let first = ellipse.points[0];
    let last = *ellipse.points.last().unwrap();
    assert!((first.x - last.x).abs() < 1e-6 && (first.y - last.y).abs() < 1e-6);
    let min_x = ellipse.points.iter().fold(f64::INFINITY, |m, p| m.min(p.x));
    let max_x = ellipse
        .points
        .iter()
        .fold(f64::NEG_INFINITY, |m, p| m.max(p.x));
    let min_y = ellipse.points.iter().fold(f64::INFINITY, |m, p| m.min(p.y));
    let max_y = ellipse
        .points
        .iter()
        .fold(f64::NEG_INFINITY, |m, p| m.max(p.y));
    assert!((max_x - min_x - 225.0).abs() < 1e-6);
    assert!((max_y - min_y - 75.0).abs() < 1e-6);

    app.document.add_layer_above_active();
    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse_shift(400.0, 200.0)), &view);
    let circle = &app.document.active_layer().strokes[0];
    let min_x = circle.points.iter().fold(f64::INFINITY, |m, p| m.min(p.x));
    let max_x = circle
        .points
        .iter()
        .fold(f64::NEG_INFINITY, |m, p| m.max(p.x));
    let min_y = circle.points.iter().fold(f64::INFINITY, |m, p| m.min(p.y));
    let max_y = circle
        .points
        .iter()
        .fold(f64::NEG_INFINITY, |m, p| m.max(p.y));
    assert!(((max_x - min_x) - (max_y - min_y)).abs() < 1e-6);
    assert!((max_x - min_x - 75.0).abs() < 1e-6);
}

#[test]
fn degenerate_shapes_are_discarded() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    app.settings.tool = Tool::Line;
    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse(100.0, 100.0)), &view);
    assert!(app.document.active_layer().strokes.is_empty());
    assert!(!app.is_dirty());
    assert!(!app.is_drawing());

    app.settings.tool = Tool::Rect;
    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse(300.0, 100.0)), &view);
    assert!(app.document.active_layer().strokes.is_empty());

    app.settings.tool = Tool::Ellipse;
    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);
    app.handle_input(InputEvent::Cancel, &view);
    assert!(app.document.active_layer().strokes.is_empty());
}

#[test]
fn finish_draft_commits_in_progress_stroke_before_export() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    app.handle_input(InputEvent::Down(pen(100.0, 100.0, 0.4)), &view);
    app.handle_input(InputEvent::Move(pen(200.0, 120.0, 0.9)), &view);

    // 手势未结束时笔迹只在草稿里，图层还是空的。
    assert!(app.is_drawing());
    assert!(app.draft_stroke().is_some());
    assert!(app.document.active_layer().strokes.is_empty());

    // 导出/保存前调用 finish_draft：草稿落盘，笔迹与压感宽度都在。
    assert!(app.finish_draft());
    let strokes = &app.document.active_layer().strokes;
    assert_eq!(strokes.len(), 1);
    assert_eq!(strokes[0].points.len(), 2);
    let widths = strokes[0]
        .widths_pt
        .as_ref()
        .expect("压感笔迹应保存逐点宽度");
    assert_eq!(widths.len(), 2);
    assert!(widths[1] > widths[0], "重压处应更宽: {widths:?}");
    assert!(app.is_dirty());
    assert!(!app.is_drawing());

    // 没有草稿时再调用是幂等的，不会重复落笔。
    assert!(!app.finish_draft());
    assert_eq!(app.document.active_layer().strokes.len(), 1);
}

#[test]
fn finish_draft_discards_empty_shape_without_marking_dirty() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    app.settings.tool = Tool::Line;
    app.handle_input(InputEvent::Down(mouse(100.0, 100.0)), &view);

    // 只有起点、没有位移的直线在提交时被丢弃。
    assert!(!app.finish_draft());
    assert!(app.document.active_layer().strokes.is_empty());
    assert!(!app.is_dirty());
    assert!(!app.is_drawing());
}

/// 模拟平台层在事件缺少压感轴时的产物：pressure 回填 1.0，但标记为“不可用”。
fn pen_without_pressure_event(x: f64, y: f64) -> PointerSample {
    let mut sample = pen(x, y, 1.0);
    sample.pressure_available = false;
    sample
}

#[test]
fn stroke_starting_without_pressure_upgrades_instead_of_full_width() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);
    // 用户报告问题时的参数
    app.settings.width_pt = 3.7;
    app.settings.pressure_gamma = 1.21;
    app.settings.min_pressure_ratio = 0.13;

    // 起笔事件没带压感轴（真机上偶发），整笔不应退化成满宽 3.7pt
    app.handle_input(
        InputEvent::Down(pen_without_pressure_event(100.0, 100.0)),
        &view,
    );
    // 后续事件带回压感：应升级为变宽笔迹并回填已有点
    app.handle_input(InputEvent::Move(pen(200.0, 100.0, 0.4)), &view);
    app.handle_input(InputEvent::Up(pen(300.0, 100.0, 0.4)), &view);

    let stroke = &app.document.active_layer().strokes[0];
    let widths = stroke.widths_pt.as_ref().expect("应升级为压感笔迹");
    assert_eq!(widths.len(), stroke.points.len());
    let expected = 3.7 * (0.13 + 0.87 * 0.4f64.powf(1.21));
    for width in widths {
        assert!(
            (width - expected).abs() < 1e-6,
            "宽度 {width} 应等于 {expected}，而不是满宽 3.7"
        );
    }
    assert!(widths.iter().all(|width| *width < 3.7 - 1e-9));
}

#[test]
fn missing_pressure_event_reuses_last_reading() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);
    app.settings.width_pt = 3.7;
    app.settings.pressure_gamma = 1.21;
    app.settings.min_pressure_ratio = 0.13;

    app.handle_input(InputEvent::Down(pen(100.0, 100.0, 0.9)), &view);
    // 中途某个事件没带压感轴：沿用上一次 0.9，不能跳到满宽
    app.handle_input(
        InputEvent::Move(pen_without_pressure_event(200.0, 100.0)),
        &view,
    );
    app.handle_input(InputEvent::Up(pen(300.0, 100.0, 0.9)), &view);

    let stroke = &app.document.active_layer().strokes[0];
    let widths = stroke.widths_pt.as_ref().unwrap();
    let expected = 3.7 * (0.13 + 0.87 * 0.9f64.powf(1.21));
    for width in widths {
        assert!(
            (width - expected).abs() < 1e-6,
            "宽度 {width} 应为 {expected}（沿用上一次压感 0.9）"
        );
    }
}

#[test]
fn mouse_stroke_stays_constant_even_after_pen_pressure_seen() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);

    // 先用笔写一笔，留下 last_pressure
    app.handle_input(InputEvent::Down(pen(100.0, 100.0, 0.9)), &view);
    app.handle_input(InputEvent::Up(pen(120.0, 120.0, 0.9)), &view);
    assert!(app.document.active_layer().strokes[0].widths_pt.is_some());

    // 再用鼠标画：无压感轴，必须保持常量宽
    app.handle_input(InputEvent::Down(mouse(200.0, 200.0)), &view);
    app.handle_input(InputEvent::Move(mouse(260.0, 200.0)), &view);
    app.handle_input(InputEvent::Up(mouse(320.0, 200.0)), &view);

    let strokes = &app.document.active_layer().strokes;
    assert_eq!(strokes.len(), 2);
    assert!(
        strokes[1].widths_pt.is_none(),
        "鼠标笔迹不应因为之前用过笔就变成压感笔迹"
    );
}

// ---- 撤回（Ctrl+Z） ----

fn draw_stroke(app: &mut App, view: &ViewTransform, y: f64) {
    app.handle_input(InputEvent::Down(mouse(50.0, y)), view);
    app.handle_input(InputEvent::Move(mouse(150.0, y)), view);
    app.handle_input(InputEvent::Up(mouse(250.0, y)), view);
}

#[test]
fn undo_removes_the_last_stroke() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);
    draw_stroke(&mut app, &view, 100.0);
    draw_stroke(&mut app, &view, 200.0);
    assert_eq!(app.document.active_layer().strokes.len(), 2);

    let first = app.document.active_layer().strokes[0].clone();

    let result = app.undo().expect("应能撤回");
    assert_eq!(result.layer, 0);
    assert!(!result.restored, "撤回新增笔迹不是恢复操作");
    assert_eq!(
        app.document.active_layer().strokes,
        vec![first],
        "剩下的是先画的那一笔"
    );
}

#[test]
fn undo_stops_at_the_history_limit() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);
    const TOTAL: usize = 55; // 超过 50 条上限
    for index in 0..TOTAL {
        draw_stroke(&mut app, &view, 20.0 + index as f64 * 5.0);
    }
    assert_eq!(app.document.active_layer().strokes.len(), TOTAL);
    assert_eq!(app.undo_depth(), 50, "最多保留 50 步");

    let mut undone = 0;
    while app.undo().is_some() {
        undone += 1;
    }
    assert_eq!(undone, 50);
    // 最早的 5 笔已经不能被撤回
    assert_eq!(app.document.active_layer().strokes.len(), TOTAL - 50);
}

#[test]
fn undo_after_erase_restores_the_stroke() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);
    draw_stroke(&mut app, &view, 100.0);
    assert_eq!(app.document.active_layer().strokes.len(), 1);

    // 用橡皮划一整条横线把笔迹擦掉
    app.settings.tool = Tool::Eraser;
    app.handle_input(InputEvent::Down(mouse(50.0, 100.0)), &view);
    app.handle_input(InputEvent::Move(mouse(150.0, 100.0)), &view);
    app.handle_input(InputEvent::Up(mouse(250.0, 100.0)), &view);
    assert_eq!(app.document.active_layer().strokes.len(), 0, "擦除生效");

    let result = app.undo().expect("应能恢复被擦掉的笔迹");
    assert!(result.restored);
    assert_eq!(app.document.active_layer().strokes.len(), 1);
}

#[test]
fn undo_on_empty_history_is_a_no_op() {
    let (_view, document) = view_and_document();
    let mut app = App::new(document);
    assert!(app.undo().is_none());
    assert!(!app.is_dirty(), "没有可撤回内容时不应把文档标脏");
}

#[test]
fn undo_works_across_layers() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);
    draw_stroke(&mut app, &view, 100.0); // 图层 1

    app.document.add_layer_above_active(); // 切到图层 2
    assert_eq!(app.document.active, 1);
    draw_stroke(&mut app, &view, 200.0);

    assert!(app.undo().is_some());
    assert_eq!(
        app.document.layers[1].strokes.len(),
        0,
        "撤的是当前层那一笔"
    );
    assert_eq!(app.document.layers[0].strokes.len(), 1, "另一层不受影响");

    // 继续撤回会把图层 1 的笔迹也撤掉（即使当前停留在图层 2）
    assert!(app.undo().is_some());
    assert_eq!(app.document.layers[0].strokes.len(), 0);
}

#[test]
fn undoing_mid_stroke_discards_the_draft_only() {
    let (view, document) = view_and_document();
    let mut app = App::new(document);
    draw_stroke(&mut app, &view, 100.0);
    assert_eq!(app.document.active_layer().strokes.len(), 1);

    // 正在画第二笔
    app.handle_input(InputEvent::Down(mouse(50.0, 200.0)), &view);
    app.handle_input(InputEvent::Move(mouse(120.0, 200.0)), &view);
    assert!(app.is_drawing());

    assert!(app.undo().is_none(), "画到一半先丢弃草稿");
    assert!(!app.is_drawing());
    assert_eq!(
        app.document.active_layer().strokes.len(),
        1,
        "已提交的那一笔不应被撤掉"
    );
}
