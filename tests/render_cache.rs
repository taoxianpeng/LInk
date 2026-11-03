mod common;

use common::TestTarget;
use link_ink::cache::OverlayCache;
use link_ink::model::{Document, Point, Rgba, Stroke};
use link_ink::paint::RectF;
use link_ink::platform::{MonitorInfo, RectF as LogicalRect, RectI};
use link_ink::view::ViewTransform;

/// 逻辑 100x50、物理 200x100（2x）。
fn monitor() -> MonitorInfo {
    MonitorInfo {
        physical: RectI {
            x: 0,
            y: 0,
            width: 200,
            height: 100,
        },
        logical: LogicalRect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
        },
        scale: 2.0,
    }
}

/// 页面 75x37.5pt ⇒ pt_to_logical = 1.3333，页面原点与屏幕原点重合。
fn document() -> Document {
    Document::new(75.0, 37.5)
}

fn view() -> ViewTransform {
    let monitor = monitor();
    ViewTransform::for_monitor(&monitor, 75.0, 37.5)
}

/// 覆盖层绘制用的视图：坐标是**物理像素**（画布后端统一约定）。
fn physical_view() -> ViewTransform {
    view().to_physical_pixels(2.0)
}

fn full_clip() -> RectF {
    RectF::new(0.0, 0.0, 200.0, 100.0)
}

/// 把缓存贴到一张 200x100 的表面，返回表面供读像素。
fn paint(cache: &OverlayCache) -> TestTarget {
    let mut target = TestTarget::new(200, 100);
    target.canvas(|canvas| cache.paint(canvas));
    target
}

/// 空白画布（等价旧测试里的 Source 清屏）。
fn blank_target() -> TestTarget {
    let mut target = TestTarget::new(200, 100);
    target.clear();
    target
}

fn horizontal_stroke(y_pt: f64, color: Rgba) -> Stroke {
    Stroke::constant(
        color,
        3.0,
        vec![Point::new(5.0, y_pt), Point::new(30.0, y_pt)],
    )
}

#[test]
fn cache_renders_committed_strokes_at_physical_resolution() {
    let mut document = document();
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(5.0, Rgba::BLACK));

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    cache.sync(&document, &physical_view());
    let target = paint(&cache);

    // 页面 (15,5)pt → 逻辑 (20, 6.67) → 物理 (40, 13)
    let hit = target.pixel_at(40, 13);
    assert_eq!(hit[3], 255, "笔迹处应不透明: {hit:?}");
    assert!(
        hit[0] < 40 && hit[1] < 40 && hit[2] < 40,
        "应为黑色: {hit:?}"
    );

    let blank = target.pixel_at(150, 80);
    assert_eq!(blank[3], 0, "空白处应透明: {blank:?}");
}

#[test]
fn cache_appends_new_stroke_without_losing_previous_ones() {
    let mut document = document();
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(5.0, Rgba::BLACK));

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    cache.sync(&document, &physical_view());

    // 再追加一笔（模拟落笔提交），应增量画进缓存
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(25.0, Rgba::BLACK));
    cache.sync(&document, &physical_view());
    let target = paint(&cache);

    let first = target.pixel_at(40, 13);
    let second = target.pixel_at(40, 66); // 页面 y=25 → 逻辑 33.3 → 物理 66.7
    assert_eq!(first[3], 255, "第一笔不应丢失: {first:?}");
    assert_eq!(second[3], 255, "新笔迹应出现在缓存里: {second:?}");
}

#[test]
fn cache_rebuilds_when_strokes_are_removed() {
    let mut document = document();
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(5.0, Rgba::BLACK));

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    cache.sync(&document, &physical_view());

    document.active_layer_mut().strokes.clear();
    cache.sync(&document, &physical_view());
    let target = paint(&cache);
    assert_eq!(
        target.pixel_at(40, 13)[3],
        0,
        "擦除后缓存不应残留笔迹"
    );
}

#[test]
fn cache_rebuilds_on_layer_order_change() {
    let mut document = document();
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(10.0, Rgba::new(255, 0, 0, 255)));
    document.add_layer_above_active();
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(10.0, Rgba::new(0, 0, 255, 255)));

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    cache.sync(&document, &physical_view());
    let target = paint(&cache);
    let top = target.pixel_at(40, 26); // 页面 y=10 → 物理 26.7
    assert!(top[2] > 200 && top[0] < 60, "上层蓝色应盖住红色: {top:?}");

    document.layers.swap(0, 1);
    cache.sync(&document, &physical_view());
    let target = paint(&cache);
    let top = target.pixel_at(40, 26);
    assert!(
        top[0] > 200 && top[2] < 60,
        "交换图层后应变成红色在上: {top:?}"
    );
}

#[test]
fn invalidate_forces_rebuild_even_when_structure_looks_same() {
    let mut document = document();
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(5.0, Rgba::BLACK));

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    cache.sync(&document, &physical_view());

    // 结构相同但内容不同（加载了另一个工程）：必须靠 invalidate 失效
    document.active_layer_mut().strokes.clear();
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(25.0, Rgba::BLACK));
    cache.invalidate();
    cache.sync(&document, &physical_view());

    let target = paint(&cache);
    assert_eq!(target.pixel_at(40, 13)[3], 0, "旧位置应被清掉");
    assert_eq!(target.pixel_at(40, 66)[3], 255, "新位置应有笔迹");
}

fn pen(x: f64, y: f64) -> link_ink::input::PointerSample {
    link_ink::input::PointerSample {
        logical: (x, y),
        pressure: 0.6,
        pressure_available: true,
        device_has_pressure: true,
        tool: link_ink::input::ToolKind::Pen,
        device_name: "测试笔".to_string(),
        tilt: None,
        shift: false,
    }
}

/// 缓存路径（贴图 + 草稿）必须和“每帧重画全部笔迹”的结果一致。
#[test]
fn cached_overlay_draw_matches_direct_rendering() {
    use link_ink::input::InputEvent;

    let mut document = document();
    // 多图层 + 半透明 + 重叠，覆盖各种合成情况
    document.layers[0]
        .strokes
        .push(horizontal_stroke(5.0, Rgba::new(255, 0, 0, 128)));
    document.layers[0]
        .strokes
        .push(horizontal_stroke(6.0, Rgba::BLACK));
    document.add_layer_above_active();
    document.layers[1]
        .strokes
        .push(horizontal_stroke(7.0, Rgba::new(0, 0, 255, 200)));

    let mut app = link_ink::app::App::new(document);
    let view = view();
    let physical = physical_view();
    // 造一个未提交的草稿
    app.handle_input(InputEvent::Down(pen(20.0, 30.0)), &view);
    app.handle_input(InputEvent::Move(pen(70.0, 35.0)), &view);

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    let mut cached = blank_target();
    cached.canvas(|canvas| {
        link_ink::ui::overlay::draw(canvas, &app, &physical, &mut cache, None, full_clip());
    });

    let mut direct = blank_target();
    direct.canvas(|canvas| {
        link_ink::render::draw_document(canvas, &app.document, &physical);
        if let Some(draft) = app.draft_stroke() {
            link_ink::render::draw_stroke(canvas, &draft, &physical);
        }
    });

    let (a, b) = (cached.rgba(), direct.rgba());
    assert_eq!(a.len(), b.len());
    let mut worst = 0i32;
    for (x, y) in a.iter().zip(b.iter()) {
        worst = worst.max((*x as i32 - *y as i32).abs());
    }
    // 两个后端（cairo / tiny-skia）的抗锯齿实现不同，允许小误差。
    #[cfg(target_os = "linux")]
    let tolerance = 2;
    #[cfg(not(target_os = "linux"))]
    let tolerance = 4;
    assert!(
        worst <= tolerance,
        "缓存绘制与直接重绘不一致，最大通道差 {worst}"
    );
}

#[test]
fn unchanged_document_does_not_rebuild_cache() {
    let mut document = document();
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(5.0, Rgba::BLACK));

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    cache.sync(&document, &physical_view());
    assert_eq!(cache.rebuilds(), 1, "首帧应建一次");

    // 指针移动的帧：文档没变，不能重建（否则每帧 30ms+，比优化前还慢）
    for _ in 0..10 {
        cache.sync(&document, &physical_view());
    }
    assert_eq!(cache.rebuilds(), 1, "无变化时不应重建");

    // 落笔提交：增量追加，仍然不重建
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(25.0, Rgba::BLACK));
    cache.sync(&document, &physical_view());
    assert_eq!(cache.rebuilds(), 1, "追加笔迹应走增量，不重建");

    // 擦除：必须重建
    document.active_layer_mut().strokes.clear();
    cache.sync(&document, &physical_view());
    assert_eq!(cache.rebuilds(), 2, "擦除后必须重建");
}

#[test]
fn hidden_layer_is_not_composited() {
    let mut document = document();
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(5.0, Rgba::BLACK));
    document.set_layer_visible(0, false);

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    cache.sync(&document, &physical_view());
    let target = paint(&cache);
    assert_eq!(
        target.pixel_at(40, 13)[3],
        0,
        "隐藏图层不应出现在覆盖层缓存里"
    );
}

#[test]
fn toggling_visibility_rebuilds_cache() {
    let mut document = document();
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(5.0, Rgba::BLACK));

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    cache.sync(&document, &physical_view());
    assert_eq!(cache.rebuilds(), 1);

    document.set_layer_visible(0, false);
    cache.sync(&document, &physical_view());
    assert_eq!(cache.rebuilds(), 2, "可见性变化必须重建缓存");

    let target = paint(&cache);
    assert_eq!(target.pixel_at(40, 13)[3], 0);
}

/// 草稿必须画在"它自己那一层"的位置：在底层书写时，上层笔迹要盖住草稿。
///
/// 回归用例：早先草稿永远画在所有图层之上，导致在非顶层书写时笔迹会浮在上面，
/// 只有提交（重新合成缓存）后才跳回正确位置，表现为"画的时候在上、松手就跑到下面"。
#[test]
fn draft_on_lower_layer_is_covered_by_upper_layers() {
    use link_ink::input::InputEvent;

    let mut document = document();
    // 上层（index 1）放一条黑色横线，横跨即将书写的区域
    document.add_layer_above_active();
    document.layers[1]
        .strokes
        .push(horizontal_stroke(15.0, Rgba::BLACK));
    // 回到下层书写
    document.active = 0;

    let mut app = link_ink::app::App::new(document);
    app.settings.color = Rgba::new(255, 0, 0, 255); // 红色草稿，便于区分
    let view = view();
    let physical = physical_view();

    // 起笔并移动，草稿仍在绘制中。
    // 页面 75x37.5pt、逻辑 100x50 ⇒ 1.3333 倍：上层黑线在页面 y=15pt，对应逻辑 y=20。
    app.handle_input(InputEvent::Down(pen(20.0, 20.0)), &view);
    app.handle_input(InputEvent::Move(pen(60.0, 20.0)), &view);
    assert!(app.is_drawing(), "应处于绘制中");
    assert_eq!(app.document.active, 0);

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    let target = {
        let mut target = blank_target();
        target.canvas(|canvas| {
            link_ink::ui::overlay::draw(canvas, &app, &physical, &mut cache, None, full_clip());
        });
        target
    };

    // 逻辑坐标 (20,20) → 物理 (40,40)；上层黑线在这里，必须盖住红色草稿
    let pixel = target.pixel_at(40, 40);
    assert!(
        pixel[0] < 60 && pixel[1] < 60 && pixel[2] < 60,
        "上层笔迹应盖住草稿，实际像素 {pixel:?}（红色说明草稿浮到了上面）"
    );
}

/// 连续两帧：形状工具的预览每帧都在变，第二帧必须把第一帧的图形擦干净。
///
/// 回归用例：离屏组里没有笔迹的地方是透明的，若用 `Over` 贴回窗口，
/// 上一帧的旧图形不会被覆盖，拖动矩形/椭圆时就会拖出一片残影。
#[test]
fn shape_preview_does_not_leave_ghosts_between_frames() {
    use link_ink::input::InputEvent;

    let mut app = link_ink::app::App::new(document());
    app.settings.tool = link_ink::app::Tool::Rect;
    app.settings.color = Rgba::BLACK;
    let view = view();
    let physical = physical_view();

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    let mut target = blank_target();

    // 第一帧：从 (10,10) 拖到 (40,25) —— 一个横向大矩形
    app.handle_input(InputEvent::Down(pen(10.0, 10.0)), &view);
    app.handle_input(InputEvent::Move(pen(40.0, 25.0)), &view);
    let damage_first = app.draft_damage_pt().expect("第一帧应有脏矩形");
    target.canvas(|canvas| {
        link_ink::ui::overlay::draw(
            canvas,
            &app,
            &physical,
            &mut cache,
            Some(damage(damage_first, &physical)),
            full_clip(),
        );
    });
    // 矩形从逻辑 (10,10) 拖到 (40,25)，物理坐标就是 (20,20)-(80,50)。
    // 探测矩形右边中点 (80,35)：描边以路径为中心，这里必有墨。
    // 第一帧逻辑矩形 (10,10)-(40,25) → 页面 (7.5,7.5)-(30,18.75) → 设备 (20,20)-(80,50)。
    // 探测右边缘中点 (80,35)：第一帧有墨；第二帧缩回 x<40 后这块必须被擦掉。
    let probe = (80u32, 35u32);
    assert!(
        is_dark(target.pixel_at(probe.0, probe.1)),
        "第一帧应画出矩形，探测点 {probe:?}"
    );

    // 第二帧：缩回到 (12,12)-(20,18)，第一帧那片区域必须被擦掉
    app.handle_input(InputEvent::Move(pen(20.0, 18.0)), &view);
    let damage_second = app.draft_damage_pt().expect("第二帧应有脏矩形");
    // 按真实链路那样取两帧的并集（窗口会把多个事件合并成一次绘制）
    let union = (
        damage_first.0.min(damage_second.0),
        damage_first.1.min(damage_second.1),
        damage_first.2.max(damage_second.2),
        damage_first.3.max(damage_second.3),
    );
    target.canvas(|canvas| {
        link_ink::ui::overlay::draw(
            canvas,
            &app,
            &physical,
            &mut cache,
            Some(damage(union, &physical)),
            full_clip(),
        );
    });

    let ghost = target.pixel_at(probe.0, probe.1);
    assert!(
        !is_dark(ghost),
        "第一帧的矩形应当被擦除，实际残留像素 {ghost:?}（残影）"
    );
}

fn is_dark(pixel: [u8; 4]) -> bool {
    pixel[3] > 128 && pixel[0] < 90 && pixel[1] < 90 && pixel[2] < 90
}

/// 页面坐标 → 设备像素坐标（`overlay::draw` 的 damage 参数用设备像素）。
fn damage(page: (f64, f64, f64, f64), view: &ViewTransform) -> (f64, f64, f64, f64) {
    let a = view.page_to_logical(Point::new(page.0, page.1));
    let b = view.page_to_logical(Point::new(page.2, page.3));
    (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

