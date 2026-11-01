use std::cell::Cell;
use std::time::Instant;

use cairo::Context;

use crate::app::App;
use crate::cache::OverlayCache;
use crate::render;
use crate::view::ViewTransform;

thread_local! {
    static LAST_FRAME: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// 只有脏区域不太大时才走「离屏组 + 一次贴图」的原子合成。
///
/// cairo 的 group 会按裁剪区大小分配中间表面：小区域几乎零成本（实测每帧 0.3~0.5ms），
/// 但整屏重绘时等于多分配 43MB。整屏重绘是图层操作、导出恢复这类一次性动作，
/// 不会连续闪烁，直接绘制更划算。
const ATOMIC_COMPOSE_MAX_AREA: f64 = 400_000.0;

/// `damage` 是这一帧需要重绘的逻辑矩形（x0,y0,x1,y1）。
///
/// GTK 在带合成器的透明窗口上往往把整屏都标记为脏，若照单全收就会每帧全屏重绘，
/// 长笔迹时能在屏幕上看到"清屏 → 重画"的间隙（闪烁）。所以这里按自己的脏矩形裁剪，
/// 只在必要时才画整个窗口。
pub fn draw(
    cr: &Context,
    app: &App,
    view: &ViewTransform,
    cache: &mut OverlayCache,
    damage: Option<(f64, f64, f64, f64)>,
) {
    let started = Instant::now();
    let previous = LAST_FRAME.with(|slot| slot.replace(Some(started)));
    let draft = app.draft_stroke();

    cache.sync(&app.document, view);

    // 关键：把「贴缓存 + 画草稿」合成到一个离屏组，最后一次性贴到窗口。
    // 否则这两步之间（清掉草稿区域 → 重画草稿）如果被合成器采样，
    // 屏幕上会看到笔迹瞬间消失 —— 笔画越长、这一帧的耗时越久，闪得越明显。
    let clip_area = cr
        .clip_extents()
        .map(|(x0, y0, x1, y1)| ((x1 - x0) * (y1 - y0)).abs())
        .unwrap_or(f64::INFINITY);

    // 只在小范围脏矩形上动手；没有提示或区域过大时才画整个裁剪区。
    let mut area = clip_area;
    if let Some((x0, y0, x1, y1)) = damage {
        let width = (x1 - x0).max(1.0);
        let height = (y1 - y0).max(1.0);
        let damage_area = width * height;
        if damage_area <= ATOMIC_COMPOSE_MAX_AREA && damage_area < clip_area {
            cr.rectangle(x0, y0, width, height);
            cr.clip();
            area = damage_area;
        }
    }

    // 草稿要夹在它自己那一层的位置：先画草稿，再把**当前层之上**的笔迹盖回来。
    let cull_pt = damage.map(|(x0, y0, x1, y1)| {
        let a = view.logical_to_page(x0, y0);
        let b = view.logical_to_page(x1, y1);
        let (ax, ay) = (a.x, a.y);
        let (bx, by) = (b.x, b.y);
        (ax.min(bx), ay.min(by), ax.max(bx), ay.max(by))
    });
    let active = app.document.active;
    let paint_draft = |cr: &Context| {
        if let Some(draft) = &draft {
            let _ = render::draw_stroke(cr, draft, view);
            let _ = render::draw_layers_above(cr, &app.document, active, view, cull_pt);
        }
        let selection = app.overlay_strokes();
        if !selection.is_empty() {
            let _ = render::draw_selection(cr, &selection, view, app.selection_transform());
        }
        if let Some(bounds) = app.selection_box() {
            let _ = render::draw_selection_box(cr, bounds, app.selection_transform(), view);
        }
        if let Some((kind, start, current, points)) = app.marquee_view() {
            let _ = render::draw_marquee(cr, kind, start, current, points, view);
        }
    };

    if area > ATOMIC_COMPOSE_MAX_AREA {
        // 大区域：直接画，少一份中间表面。
        let _ = cache.paint(cr);
        cr.set_operator(cairo::Operator::Over);
        paint_draft(cr);
    } else {
        cr.push_group();
        // 组内先放已提交笔迹（Source 覆盖，顺带清掉上一帧草稿），再叠当前草稿。
        let _ = cache.paint(cr);
        cr.set_operator(cairo::Operator::Over);
        paint_draft(cr);
        if cr.pop_group_to_source().is_ok() {
            // 关键：必须用 Source 把整块裁剪区替换掉。
            // 组里没有笔迹的地方是**透明**的，用 Over 贴回去时这些像素不会被覆盖，
            // 上一帧的图形就留在了屏幕上 —— 形状工具拖动时的一片残影就是这么来的
            // （自由笔迹一直是向外长，新旧完全重叠，所以看不出来）。
            cr.set_operator(cairo::Operator::Source);
            let _ = cr.paint();
        }
    }

    if log::log_enabled!(log::Level::Trace) {
        log::trace!(
            "overlay frame: 裁剪面积 {area:.0}（原子={}），间隔 {:?}，耗时 {:.2}ms，草稿 {} 点，缓存重建 {} 次",
            area <= ATOMIC_COMPOSE_MAX_AREA,
            previous.map(|instant| started.duration_since(instant)),
            started.elapsed().as_secs_f64() * 1000.0,
            draft
                .as_ref()
                .map(|stroke| stroke.points.len())
                .unwrap_or(0),
            cache.rebuilds(),
        );
    }
}
