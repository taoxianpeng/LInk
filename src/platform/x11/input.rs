use gtk::gdk;
use gtk::gdk::prelude::*;

use crate::input::{clamp_pressure, PointerSample, ToolKind};

pub fn sample_from_event(event: &gdk::Event, logical: (f64, f64)) -> PointerSample {
    let device = event.device();
    let device_name = device
        .as_ref()
        .and_then(|d| d.name())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "未知设备".to_string());

    let tool = event
        .device_tool()
        .map(|t| tool_from_tool_type(t.tool_type()))
        .or_else(|| device.as_ref().map(|d| tool_from_source(d.source())))
        .unwrap_or(ToolKind::Mouse);

    let (pressure, pressure_available) = clamp_pressure(event.axis(gdk::AxisUse::Pressure));
    let device_has_pressure = device.as_ref().map(has_pressure_axis).unwrap_or(false);
    let tilt = match (
        event.axis(gdk::AxisUse::Xtilt),
        event.axis(gdk::AxisUse::Ytilt),
    ) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some((x, y)),
        _ => None,
    };
    let shift = event
        .state()
        .is_some_and(|s| s.contains(gdk::ModifierType::SHIFT_MASK));

    PointerSample {
        logical,
        pressure,
        pressure_available,
        device_has_pressure,
        tool,
        device_name,
        tilt,
        shift,
    }
}

pub fn has_pressure_axis(device: &gdk::Device) -> bool {
    let n = device.n_axes();
    (0..n).any(|i| device.axis_use(i as u32) == gdk::AxisUse::Pressure)
}

fn tool_from_tool_type(tool_type: gdk::DeviceToolType) -> ToolKind {
    match tool_type {
        gdk::DeviceToolType::Eraser => ToolKind::Eraser,
        gdk::DeviceToolType::Pen
        | gdk::DeviceToolType::Brush
        | gdk::DeviceToolType::Pencil
        | gdk::DeviceToolType::Airbrush => ToolKind::Pen,
        gdk::DeviceToolType::Mouse | gdk::DeviceToolType::Lens => ToolKind::Mouse,
        _ => ToolKind::Pen,
    }
}

fn tool_from_source(source: gdk::InputSource) -> ToolKind {
    match source {
        gdk::InputSource::Eraser => ToolKind::Eraser,
        gdk::InputSource::Pen => ToolKind::Pen,
        _ => ToolKind::Mouse,
    }
}
