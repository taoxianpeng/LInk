use std::cell::Cell;
use std::time::Instant;

use crate::app::App;
use crate::cache::OverlayCache;
use crate::paint::{Blend, Canvas, RectF};
use crate::render;
use crate::view::ViewTransform;

thread_local! {
    static LAST_FRAME: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// 只有脏区域不太大时才走「离屏组 + 一次贴图」的原子合成。
///
/// 离屏组会按裁剪区大小分配中间表面：小区域几乎零成本（实测每帧 0.3~0.5ms），
/// 但整屏重绘时等于多分配几十 MB。整屏重绘是图层操作、导出恢复这类一次性动作，
/// 不会连续闪烁，直接绘制更划算。
const ATOMIC_COMPOSE_MAX_AREA: f64 = 400_000.0;

/// `damage` 是这一帧需要重绘的逻辑矩形（x0,y0,x1,y1），与 `clip` 同为**设备像素**。
///
/// 窗口层在带合成器的透明窗口上往往把整屏都标记为脏，若照单全收就会每帧全屏重绘，
/// 长笔迹时能在屏幕上看到"清屏 → 重画"的间隙（闪烁）。所以这里按自己的脏矩形裁剪，
/// 只在必要时才画整个窗口。
pub fn draw(
    canvas: &mut dyn Canvas,
    app: &App,
    view: &ViewTransform,
    cache: &mut OverlayCache,
    damage: Option<(f64, f64, f64, f64)>,
    clip: RectF,
) {
    let started = Instant::now();
    let previous = LAST_FRAME.with(|slot| slot.replace(Some(started)));
    let draft = app.draft_stroke();

    cache.sync(&app.document, view);

    let clip_area = (clip.width * clip.height).abs();

    // 只在小范围脏矩形上动手；没有提示或区域过大时才画整个裁剪区。
    let mut area = clip_area;
    if let Some((x0, y0, x1, y1)) = damage {
        let width = (x1 - x0).max(1.0);
        let height = (y1 - y0).max(1.0);
        let damage_area = width * height;
        if damage_area <= ATOMIC_COMPOSE_MAX_AREA && damage_area < clip_area {
            canvas.clip_rect(x0, y0, width, height);
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
    let paint_draft = |canvas: &mut dyn Canvas| {
        if let Some(draft) = &draft {
            render::draw_stroke(canvas, draft, view);
            render::draw_layers_above(canvas, &app.document, active, view, cull_pt);
        }
        let selection = app.overlay_strokes();
        if !selection.is_empty() {
            render::draw_selection(canvas, &selection, view, app.selection_transform());
        }
        if let Some(bounds) = app.selection_box() {
            render::draw_selection_box(canvas, bounds, app.selection_transform(), view);
        }
        if let Some((kind, start, current, points)) = app.marquee_view() {
            render::draw_marquee(canvas, kind, start, current, points, view);
        }
    };

    // 小区域走原子合成：组的范围必须与被裁剪的区域一致。
    //
    // cairo 的 `push_group` 会自己按当前裁剪区分配，但 Windows 后端（tiny-skia）需要
    // 显式告诉它「组覆盖哪块屏幕区域」才能把组内坐标平移回原位。这里统一用
    // `compose_bounds`：默认是整屏，按 damage 裁剪后就是脏矩形本身。
    let compose_bounds = damage
        .map(|(x0, y0, x1, y1)| RectF::from_bounds(x0, y0, x1, y1))
        .unwrap_or(clip);

    if area > ATOMIC_COMPOSE_MAX_AREA {
        // 大区域：直接画，少一份中间表面。
        cache.paint(canvas);
        paint_draft(canvas);
    } else {
        canvas.push_group(compose_bounds);
        // 组内先放已提交笔迹（Source 覆盖，顺带清掉上一帧草稿），再叠当前草稿。
        cache.paint(canvas);
        paint_draft(canvas);
        canvas.pop_group_to_source(compose_bounds);
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

/// 用纯色铺满整张画布（窗口层清屏用）。
pub fn clear(canvas: &mut dyn Canvas, color: crate::paint::Color) {
    canvas.paint_with(color, Blend::Source);
}
