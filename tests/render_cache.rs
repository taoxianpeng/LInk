use cairo::{Context, Format, ImageSurface};
use link_ink::cache::OverlayCache;
use link_ink::model::{Document, Point, Rgba, Stroke};
use link_ink::platform::{MonitorInfo, RectF, RectI};
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
        logical: RectF {
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

/// 模拟覆盖层窗口：物理 200x100 像素，设备缩放 2（用户坐标是逻辑 100x50）。
fn target() -> ImageSurface {
    let surface = ImageSurface::create(Format::ARgb32, 200, 100).expect("target");
    surface.set_device_scale(2.0, 2.0);
    surface
}

fn read_pixel(surface: &mut ImageSurface, x: u32, y: u32) -> [u8; 4] {
    let stride = surface.stride() as usize;
    let data = surface.data().expect("data");
    let offset = y as usize * stride + x as usize * 4;
    let pixel = &data[offset..offset + 4];
    // ARGB32 小端：内存序 B,G,R,A
    [pixel[2], pixel[1], pixel[0], pixel[3]]
}

fn paint(cache: &OverlayCache) -> ImageSurface {
    let surface = target();
    {
        let cr = Context::new(&surface).expect("ctx");
        cr.set_operator(cairo::Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        cr.paint().expect("clear");
        cr.set_operator(cairo::Operator::Over);
        cache.paint(&cr).expect("paint cache");
    }
    surface
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
    cache.sync(&document, &view());
    let mut surface = paint(&cache);

    // 页面 (15,5)pt → 逻辑 (20, 6.67) → 物理 (40, 13)
    let hit = read_pixel(&mut surface, 40, 13);
    assert_eq!(hit[3], 255, "笔迹处应不透明: {hit:?}");
    assert!(
        hit[0] < 40 && hit[1] < 40 && hit[2] < 40,
        "应为黑色: {hit:?}"
    );

    let blank = read_pixel(&mut surface, 150, 80);
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
    cache.sync(&document, &view());

    // 再追加一笔（模拟落笔提交），应增量画进缓存
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(25.0, Rgba::BLACK));
    cache.sync(&document, &view());
    let mut surface = paint(&cache);

    let first = read_pixel(&mut surface, 40, 13);
    let second = read_pixel(&mut surface, 40, 66); // 页面 y=25 → 逻辑 33.3 → 物理 66.7
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
    cache.sync(&document, &view());

    document.active_layer_mut().strokes.clear();
    cache.sync(&document, &view());
    let mut surface = paint(&cache);
    assert_eq!(
        read_pixel(&mut surface, 40, 13)[3],
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
    cache.sync(&document, &view());
    let mut surface = paint(&cache);
    let top = read_pixel(&mut surface, 40, 26); // 页面 y=10 → 物理 26.7
    assert!(top[2] > 200 && top[0] < 60, "上层蓝色应盖住红色: {top:?}");

    document.layers.swap(0, 1);
    cache.sync(&document, &view());
    let mut surface = paint(&cache);
    let top = read_pixel(&mut surface, 40, 26);
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
    cache.sync(&document, &view());

    // 结构相同但内容不同（加载了另一个工程）：必须靠 invalidate 失效
    document.active_layer_mut().strokes.clear();
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(25.0, Rgba::BLACK));
    cache.invalidate();
    cache.sync(&document, &view());

    let mut surface = paint(&cache);
    assert_eq!(read_pixel(&mut surface, 40, 13)[3], 0, "旧位置应被清掉");
    assert_eq!(read_pixel(&mut surface, 40, 66)[3], 255, "新位置应有笔迹");
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
    // 造一个未提交的草稿
    app.handle_input(InputEvent::Down(pen(20.0, 30.0)), &view);
    app.handle_input(InputEvent::Move(pen(70.0, 35.0)), &view);

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    let mut cached = target();
    {
        let cr = Context::new(&cached).expect("ctx");
        link_ink::ui::overlay::draw(&cr, &app, &view, &mut cache);
    }

    let mut direct = target();
    {
        let cr = Context::new(&direct).expect("ctx");
        cr.set_operator(cairo::Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        cr.paint().expect("clear");
        cr.set_operator(cairo::Operator::Over);
        link_ink::render::draw_document(&cr, &app.document, &view).expect("doc");
        if let Some(draft) = app.draft_stroke() {
            link_ink::render::draw_stroke(&cr, &draft, &view).expect("draft");
        }
    }

    let (a, b) = (read_all(&mut cached), read_all(&mut direct));
    assert_eq!(a.len(), b.len());
    let mut worst = 0i32;
    for (x, y) in a.iter().zip(b.iter()) {
        worst = worst.max((*x as i32 - *y as i32).abs());
    }
    assert!(worst <= 2, "缓存绘制与直接重绘不一致，最大通道差 {worst}");
}

fn read_all(surface: &mut ImageSurface) -> Vec<u8> {
    let stride = surface.stride() as usize;
    let width = surface.width() as usize;
    let height = surface.height() as usize;
    let data = surface.data().expect("data");
    let mut out = Vec::with_capacity(width * height * 4);
    for row in data.chunks_exact(stride).take(height) {
        out.extend_from_slice(&row[..width * 4]);
    }
    out
}

#[test]
fn unchanged_document_does_not_rebuild_cache() {
    let mut document = document();
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(5.0, Rgba::BLACK));

    let mut cache = OverlayCache::new(&monitor(), 2.0);
    cache.sync(&document, &view());
    assert_eq!(cache.rebuilds(), 1, "首帧应建一次");

    // 指针移动的帧：文档没变，不能重建（否则每帧 30ms+，比优化前还慢）
    for _ in 0..10 {
        cache.sync(&document, &view());
    }
    assert_eq!(cache.rebuilds(), 1, "无变化时不应重建");

    // 落笔提交：增量追加，仍然不重建
    document
        .active_layer_mut()
        .strokes
        .push(horizontal_stroke(25.0, Rgba::BLACK));
    cache.sync(&document, &view());
    assert_eq!(cache.rebuilds(), 1, "追加笔迹应走增量，不重建");

    // 擦除：必须重建
    document.active_layer_mut().strokes.clear();
    cache.sync(&document, &view());
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
    cache.sync(&document, &view());
    let mut surface = paint(&cache);
    assert_eq!(
        read_pixel(&mut surface, 40, 13)[3],
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
    cache.sync(&document, &view());
    assert_eq!(cache.rebuilds(), 1);

    document.set_layer_visible(0, false);
    cache.sync(&document, &view());
    assert_eq!(cache.rebuilds(), 2, "可见性变化必须重建缓存");

    let mut surface = paint(&cache);
    assert_eq!(read_pixel(&mut surface, 40, 13)[3], 0);
}
