use crate::model::Point;
use crate::platform::MonitorInfo;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewTransform {
    origin_logical: (f64, f64),
    origin_physical: (f64, f64),
    pt_to_logical: f64,
    physical_scale: f64,
}

impl ViewTransform {
    pub fn for_monitor(monitor: &MonitorInfo, page_width_pt: f64, page_height_pt: f64) -> Self {
        let avail_w = monitor.logical.width.max(1.0);
        let avail_h = monitor.logical.height.max(1.0);
        let page_w = page_width_pt.max(1.0);
        let page_h = page_height_pt.max(1.0);
        let scale = fit_scale(avail_w, avail_h, page_w, page_h);
        let origin_logical = (
            (avail_w - page_w * scale) / 2.0,
            (avail_h - page_h * scale) / 2.0,
        );
        let physical_scale = if monitor.scale.is_finite() && monitor.scale > 0.0 {
            monitor.scale
        } else {
            1.0
        };
        let origin_physical = (
            monitor.physical.x as f64 + origin_logical.0 * physical_scale,
            monitor.physical.y as f64 + origin_logical.1 * physical_scale,
        );
        Self {
            origin_logical,
            origin_physical,
            pt_to_logical: scale,
            physical_scale,
        }
    }

    pub fn for_area(
        area_width: f64,
        area_height: f64,
        page_width_pt: f64,
        page_height_pt: f64,
    ) -> Self {
        let avail_w = area_width.max(1.0);
        let avail_h = area_height.max(1.0);
        let page_w = page_width_pt.max(1.0);
        let page_h = page_height_pt.max(1.0);
        let scale = fit_scale(avail_w, avail_h, page_w, page_h);
        let origin = (
            (avail_w - page_w * scale) / 2.0,
            (avail_h - page_h * scale) / 2.0,
        );
        Self {
            origin_logical: origin,
            origin_physical: origin,
            pt_to_logical: scale,
            physical_scale: 1.0,
        }
    }

    /// 返回绘制到物理像素表面时使用的变换（逻辑坐标 × 显示器 scale）。
    pub fn to_physical_pixels(&self, scale: f64) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        Self {
            origin_logical: (self.origin_logical.0 * scale, self.origin_logical.1 * scale),
            origin_physical: (
                self.origin_physical.0 * scale,
                self.origin_physical.1 * scale,
            ),
            pt_to_logical: self.pt_to_logical * scale,
            physical_scale: self.physical_scale * scale,
        }
    }

    pub fn pt_to_logical(&self) -> f64 {
        self.pt_to_logical
    }

    pub fn physical_scale(&self) -> f64 {
        self.physical_scale
    }

    pub fn origin_logical(&self) -> (f64, f64) {
        self.origin_logical
    }

    pub fn origin_physical(&self) -> (f64, f64) {
        self.origin_physical
    }

    pub fn page_to_logical(&self, point: Point) -> (f64, f64) {
        (
            self.origin_logical.0 + point.x * self.pt_to_logical,
            self.origin_logical.1 + point.y * self.pt_to_logical,
        )
    }

    pub fn logical_to_page(&self, x: f64, y: f64) -> Point {
        Point::new(
            (x - self.origin_logical.0) / self.pt_to_logical,
            (y - self.origin_logical.1) / self.pt_to_logical,
        )
    }

    pub fn page_to_physical(&self, point: Point) -> (f64, f64) {
        (
            self.origin_physical.0 + point.x * self.pt_to_logical * self.physical_scale,
            self.origin_physical.1 + point.y * self.pt_to_logical * self.physical_scale,
        )
    }

    pub fn logical_to_physical(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.origin_physical.0 + (x - self.origin_logical.0) * self.physical_scale,
            self.origin_physical.1 + (y - self.origin_logical.1) * self.physical_scale,
        )
    }
}

fn fit_scale(avail_w: f64, avail_h: f64, page_w: f64, page_h: f64) -> f64 {
    let scale = (avail_w / page_w).min(avail_h / page_h);
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}
