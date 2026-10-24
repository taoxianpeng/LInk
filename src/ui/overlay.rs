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

pub fn draw(cr: &Context, app: &App, view: &ViewTransform, cache: &mut OverlayCache) {
    let started = Instant::now();
    let previous = LAST_FRAME.with(|slot| slot.replace(Some(started)));
    let draft = app.draft_stroke();

    cache.sync(&app.document, view);

    // 关键：把「贴缓存 + 画草稿」合成到一个离屏组，最后一次性贴到窗口。
    // 否则这两步之间（清掉草稿区域 → 重画草稿）如果被合成器采样，
    // 屏幕上会看到笔迹瞬间消失 —— 笔画越长、这一帧的耗时越久，闪得越明显。
    let area = cr
        .clip_extents()
        .map(|(x0, y0, x1, y1)| ((x1 - x0) * (y1 - y0)).abs())
        .unwrap_or(f64::INFINITY);

    if area > ATOMIC_COMPOSE_MAX_AREA {
        // 大区域：直接画，少一份中间表面。
        let _ = cache.paint(cr);
        cr.set_operator(cairo::Operator::Over);
        if let Some(draft) = &draft {
            let _ = render::draw_stroke(cr, draft, view);
        }
    } else {
        cr.push_group();
        // 组内先放已提交笔迹（Source 覆盖，顺带清掉上一帧草稿），再叠当前草稿。
        let _ = cache.paint(cr);
        cr.set_operator(cairo::Operator::Over);
        if let Some(draft) = &draft {
            let _ = render::draw_stroke(cr, draft, view);
        }
        if cr.pop_group_to_source().is_ok() {
            let _ = cr.paint();
        }
    }

    if log::log_enabled!(log::Level::Trace) {
        log::trace!(
            "overlay frame: 间隔 {:?}, 完整编译 {:.2}ms, 草稿 {} 点, 缓存重建 {} 次",
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
