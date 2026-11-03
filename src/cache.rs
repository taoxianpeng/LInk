use crate::model::Document;
use crate::paint::{new_surface, Blend, Color, Surface};
use crate::platform::MonitorInfo;
use crate::render;
use crate::view::ViewTransform;

/// 已提交笔迹的离屏缓存（按显示器物理分辨率渲染）。
///
/// 覆盖层每帧只需要「贴缓存 + 画当前草稿」。没有缓存时每帧都要重新栅格化全部笔迹，
/// 实测 4K 屏上 100 笔约 7.7ms、1000 笔约 33.7ms、3000 笔约 68.5ms——远超一帧预算，
/// 加上每帧开头要先清成透明，屏幕上就会看到画面反复闪白/丢笔迹。
///
/// 失效策略（保守但简单）：
/// - 图层数量/顺序/名称变化、任一图层笔数减少（擦除/清空）、加载工程 → 整张重建；
/// - 顶层图层笔数增加（正常落笔）→ 只把新增的那几笔画进缓存，O(1)；
/// - 非顶层图层增加笔迹 → 整张重建，保证遮挡顺序正确。
pub struct OverlayCache {
    surface: Box<dyn Surface>,
    /// 逻辑坐标 → 物理像素的缩放（GPU 缩放因子）。
    scale: f64,
    names: Vec<String>,
    counts: Vec<usize>,
    visibility: Vec<bool>,
    ready: bool,
    /// 整张重建次数（诊断/测试用：无变化时不应增长）。
    rebuilds: u64,
}

impl OverlayCache {
    /// 按主屏**物理像素**建面。
    ///
    /// 绘制坐标一律是物理像素，所以每次绘制前把调用方的逻辑视图换算到物理像素
    /// （`ViewTransform::to_physical_pixels`），后端不再需要设备缩放概念。
    pub fn new(monitor: &MonitorInfo, scale: f64) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let width = (monitor.logical.width.max(1.0) * scale).ceil() as u32;
        let height = (monitor.logical.height.max(1.0) * scale).ceil() as u32;
        let surface = new_surface(width, height).expect("创建覆盖层缓存失败");
        Self {
            surface,
            scale,
            names: Vec::new(),
            counts: Vec::new(),
            visibility: Vec::new(),
            ready: false,
            rebuilds: 0,
        }
    }

    /// 缓存的物理像素缩放因子。
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// 强制下次 `sync` 重建（加载工程等结构指纹无法区分的情况）。
    pub fn invalidate(&mut self) {
        self.ready = false;
    }

    /// 把缓存贴到目标画布。
    ///
    /// 用 Source 直接覆盖目标区域：既贴上了已提交笔迹，也把该区域里上一帧的草稿
    /// 一并抹掉，省掉一次单独的清屏（清屏会让屏幕短暂变空）。
    pub fn paint(&self, canvas: &mut dyn crate::paint::Canvas) {
        canvas.draw_surface(self.surface.as_ref(), Blend::Source);
    }

    /// 整张重建的次数（诊断/测试用）。
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds
    }

    /// 让缓存与文档一致。
    ///
    /// `view` 必须是**设备像素**视图（调用方已用 `to_physical_pixels` 换算过）；
    /// 缓存表面本身也是按物理像素分配的，两边 1:1，不能再乘一次缩放。
    ///
    /// - 结构/笔数都没变（指针移动的大多数帧）→ 什么都不做；
    /// - 顶层图层只多了笔迹 → 增量追加，O(1)；
    /// - 其余情况 → 整张重建。
    pub fn sync(&mut self, document: &Document, view: &ViewTransform) {
        if self.matches(document) {
            return;
        }
        if self.appendable(document) {
            let top = document.layers.len() - 1;
            let from = self.counts[top];
            let physical = *view;
            self.surface.with_canvas(&mut |canvas| {
                for stroke in &document.layers[top].strokes[from..] {
                    render::draw_stroke(canvas, stroke, &physical);
                }
            });
            self.snapshot(document);
            return;
        }
        self.rebuild(document, view);
        self.snapshot(document);
    }

    /// 文档结构与上次快照完全一致。
    fn matches(&self, document: &Document) -> bool {
        self.ready
            && document.layers.len() == self.counts.len()
            && self.names_matches(document)
            && self.visibility_matches(document)
            && document
                .layers
                .iter()
                .zip(self.counts.iter())
                .all(|(layer, count)| layer.strokes.len() == *count)
    }

    /// 可见性变化不需要重建整张缓存，但需要让 `matches` 判否；这里把它纳入结构比较。
    fn visibility_matches(&self, document: &Document) -> bool {
        document
            .layers
            .iter()
            .zip(self.visibility.iter())
            .all(|(layer, visible)| layer.visible == *visible)
            && document.layers.len() == self.visibility.len()
    }

    fn names_matches(&self, document: &Document) -> bool {
        document.layers.len() == self.names.len()
            && document
                .layers
                .iter()
                .zip(self.names.iter())
                .all(|(layer, name)| layer.name == *name)
    }

    /// 只有“顶层图层多了笔迹、其余完全没变”才能增量追加。
    fn appendable(&self, document: &Document) -> bool {
        if !self.ready
            || document.layers.is_empty()
            || document.layers.len() != self.counts.len()
            || !self.names_matches(document)
        {
            return false;
        }
        let top = document.layers.len() - 1;
        for (index, layer) in document.layers.iter().enumerate() {
            let count = layer.strokes.len();
            if index == top {
                if count < self.counts[top] {
                    return false;
                }
            } else if count != self.counts[index] {
                return false;
            }
        }
        document.layers[top].strokes.len() > self.counts[top]
    }

    fn snapshot(&mut self, document: &Document) {
        self.names = document
            .layers
            .iter()
            .map(|layer| layer.name.clone())
            .collect();
        self.counts = document
            .layers
            .iter()
            .map(|layer| layer.strokes.len())
            .collect();
        self.visibility = document.layers.iter().map(|layer| layer.visible).collect();
    }

    fn rebuild(&mut self, document: &Document, physical: &ViewTransform) {
        self.rebuilds += 1;
        let physical = *physical;
        self.surface.with_canvas(&mut |canvas| {
            canvas.paint_with(Color::TRANSPARENT, Blend::Source);
            render::draw_document(canvas, document, &physical);
        });
        self.ready = true;
    }
}
