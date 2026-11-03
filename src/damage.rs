//! 脏区域合并（纯逻辑，两个平台的画布后端共用）。
//!
//! 窗口层会把多个指针事件合并成**一次**绘制，所以这里必须累加：只保留最后一帧的矩形
//! 会让更早那些图形留下的像素永远没被擦掉（形状工具拖动时就是一片残影）。

/// 设备像素的矩形 `(x0, y0, x1, y1)`。
pub type DamageRect = (f64, f64, f64, f64);

/// 下一次绘制要覆盖的范围。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DrawScope {
    /// 整屏重绘（图层操作、擦除、导出恢复……），裁剪交给窗口层。
    Full,
    /// 只重绘这一块（设备像素）。
    Partial(DamageRect),
}

/// 合并两次绘制请求：并集，整屏优先。
pub fn merge_scope(previous: Option<DrawScope>, next: DrawScope) -> DrawScope {
    match (previous, next) {
        // 只要有一次要求整屏，就必须整屏。
        (Some(DrawScope::Full), _) | (_, DrawScope::Full) => DrawScope::Full,
        (Some(DrawScope::Partial(a)), DrawScope::Partial(b)) => {
            DrawScope::Partial((a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
        }
        (None, next) => next,
    }
}

/// 两个矩形取并集。
pub fn union_rect(a: Option<DamageRect>, b: Option<DamageRect>) -> Option<DamageRect> {
    match (a, b) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// 向外取整到整数像素，并保证宽高至少 1。
pub fn integer_rect(rect: DamageRect) -> (i32, i32, i32, i32) {
    let x0 = rect.0.floor() as i32 - 1;
    let y0 = rect.1.floor() as i32 - 1;
    let x1 = rect.2.ceil() as i32 + 1;
    let y1 = rect.3.ceil() as i32 + 1;
    (x0, y0, (x1 - x0).max(1), (y1 - y0).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> DrawScope {
        DrawScope::Partial((x0, y0, x1, y1))
    }

    #[test]
    fn partial_damages_accumulate_instead_of_replacing() {
        // 两个事件在一次绘制前到达：必须取并集，否则前一个图形的像素不会被擦掉。
        let first = merge_scope(None, rect(0.0, 0.0, 10.0, 10.0));
        let both = merge_scope(Some(first), rect(20.0, 20.0, 30.0, 30.0));
        assert_eq!(both, rect(0.0, 0.0, 30.0, 30.0));
    }

    #[test]
    fn full_repaint_wins_over_partial() {
        let partial = merge_scope(None, rect(0.0, 0.0, 10.0, 10.0));
        assert_eq!(merge_scope(Some(partial), DrawScope::Full), DrawScope::Full);
        assert_eq!(
            merge_scope(Some(DrawScope::Full), rect(5.0, 5.0, 6.0, 6.0)),
            DrawScope::Full
        );
    }

    #[test]
    fn first_scope_is_kept_as_is() {
        assert_eq!(
            merge_scope(None, rect(1.0, 2.0, 3.0, 4.0)),
            rect(1.0, 2.0, 3.0, 4.0)
        );
    }

    #[test]
    fn union_of_missing_rects() {
        assert_eq!(union_rect(None, None), None);
        assert_eq!(union_rect(Some((0.0, 0.0, 1.0, 1.0)), None), Some((0.0, 0.0, 1.0, 1.0)));
    }

    #[test]
    fn integer_rect_pads_by_one_pixel() {
        // floor(10.2)-1=9, floor(10.8)-1=9, ceil(20.1)+1=22, ceil(20.9)+1=22
        assert_eq!(integer_rect((10.2, 10.8, 20.1, 20.9)), (9, 9, 13, 13));
    }
}
