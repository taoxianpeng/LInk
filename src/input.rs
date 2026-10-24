#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Pen,
    Eraser,
    Mouse,
}

impl ToolKind {
    pub fn label(self) -> &'static str {
        match self {
            ToolKind::Pen => "笔",
            ToolKind::Eraser => "橡皮",
            ToolKind::Mouse => "鼠标",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PointerSample {
    pub logical: (f64, f64),
    pub pressure: f64,
    pub pressure_available: bool,
    pub device_has_pressure: bool,
    pub tool: ToolKind,
    pub device_name: String,
    pub tilt: Option<(f64, f64)>,
    pub shift: bool,
}

#[derive(Debug, Clone)]
pub enum InputEvent {
    Down(PointerSample),
    Move(PointerSample),
    Up(PointerSample),
    ProximityOut,
    Cancel,
}

pub fn clamp_pressure(value: Option<f64>) -> (f64, bool) {
    match value {
        Some(v) if v.is_finite() => (v.clamp(0.0, 1.0), true),
        _ => (1.0, false),
    }
}

pub fn pressure_width(base_width: f64, pressure: f64, gamma: f64, min_ratio: f64) -> f64 {
    let gamma = if gamma.is_finite() && gamma > 0.0 {
        gamma
    } else {
        1.0
    };
    let min_ratio = if min_ratio.is_finite() {
        min_ratio.clamp(0.0, 1.0)
    } else {
        0.2
    };
    let pressure = if pressure.is_finite() {
        pressure.clamp(0.0, 1.0)
    } else {
        1.0
    };
    base_width * (min_ratio + (1.0 - min_ratio) * pressure.powf(gamma))
}
