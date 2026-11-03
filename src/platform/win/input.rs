//! 数位笔/鼠标输入：`WM_POINTER` 消息 → [`PointerSample`]。
//!
//! 关键点：
//! - 压感只在 `penMask & PEN_MASK_PRESSURE != 0` 时有效（否则是“没有这个轴”，不是 0）；
//! - `WM_POINTERUPDATE` 会按帧合并，快速书写会缺采样点，所以用
//!   `GetPointerPenInfoHistory` 把一帧内的历史点补齐；
//! - 笔尾/橡皮擦由 `PEN_FLAG_INVERTED | PEN_FLAG_ERASER` 判断；
//! - 鼠标统一走指针通道需要启动时 `EnableMouseInPointer(TRUE)`。

use std::cell::Cell;

use crate::input::{PointerSample, ToolKind};

thread_local! {
    /// 主屏原点（屏幕物理像素）。
    static ORIGIN: Cell<(i32, i32)> = const { Cell::new((0, 0)) };
    /// 主屏缩放（物理 / 逻辑）。
    static SCALE: Cell<f64> = const { Cell::new(1.0) };
}

pub const WM_POINTER_UPDATE: u32 = 0x0245;
pub const WM_POINTER_DOWN: u32 = 0x0246;
pub const WM_POINTER_UP: u32 = 0x0247;
pub const WM_POINTER_ENTER: u32 = 0x0249;
pub const WM_POINTER_LEAVE: u32 = 0x024A;
pub const WM_POINTER_CAPTURE_CHANGED: u32 = 0x024C;

const PT_PEN: i32 = 3;

const PEN_MASK_PRESSURE: u32 = 0x0001;
const PEN_MASK_TILT_X: u32 = 0x0004;
const PEN_MASK_TILT_Y: u32 = 0x0008;
const PEN_FLAG_INVERTED: u32 = 0x0002;
const PEN_FLAG_ERASER: u32 = 0x0004;

/// 让鼠标也走 `WM_POINTER`（可选）。
///
/// 默认**不**启用：`WM_POINTER` 会把一次拖拽合并成很少几条消息，
/// 连续笔迹会退化成折线。鼠标走传统 `WM_MOUSEMOVE`，笔才走指针通道。
#[allow(dead_code)]
pub fn enable_mouse_in_pointer() {
    let enabled = unsafe { windows_sys::Win32::UI::Input::Pointer::IsMouseInPointerEnabled() };
    if enabled == 0 {
        unsafe {
            windows_sys::Win32::UI::Input::Pointer::EnableMouseInPointer(1);
        }
    }
}

/// 由窗口层在创建窗口后调用，告诉输入层主屏原点与缩放。
pub fn set_screen_mapping(origin: (i32, i32), scale: f64) {
    ORIGIN.with(|cell| cell.set(origin));
    SCALE.with(|cell| cell.set(if scale > 0.0 { scale } else { 1.0 }));
}

/// 客户区物理像素 → 画布逻辑坐标（鼠标消息用）。
pub fn physical_to_logical(x: i32, y: i32) -> (f64, f64) {
    let scale = SCALE.with(|cell| cell.get()).max(0.01);
    (x as f64 / scale, y as f64 / scale)
}

/// 屏幕物理像素 → 画布逻辑坐标。
fn screen_to_logical(x: i32, y: i32) -> (f64, f64) {
    let (ox, oy) = ORIGIN.with(|cell| cell.get());
    let scale = SCALE.with(|cell| cell.get()).max(0.01);
    ((x - ox) as f64 / scale, (y - oy) as f64 / scale)
}

/// `wParam` 低 16 位是 pointer id（`GET_POINTERID_WPARAM` 宏在 windows-sys 里没有）。
pub fn pointer_id(wparam: usize) -> u32 {
    (wparam & 0xFFFF) as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    Down,
    Move,
    Up,
    Enter,
    Leave,
    Cancel,
}

pub struct PointerReading {
    pub sample: PointerSample,
    pub phase: PointerPhase,
    pub pointer_id: u32,
    /// 同一帧内更早的采样点（时间顺序），用于补齐快速书写的缺口。
    pub history: Vec<PointerSample>,
}

/// 把一条指针消息解析成压感/倾斜/设备信息。
pub fn read_pointer(wparam: usize, phase: PointerPhase) -> Option<PointerReading> {
    let pointer_id = pointer_id(wparam);

    // `GetPointerPenInfo` 对鼠标也会成功，必须用 `GetPointerType` 区分。
    // 只有 PT_PEN 才有压感/倾斜/橡皮擦语义。
    let mut pointer_type = 0i32;
    unsafe {
        windows_sys::Win32::UI::Input::Pointer::GetPointerType(pointer_id, &mut pointer_type);
    }
    let is_pen = pointer_type == PT_PEN;
    let mut pen = windows_sys::Win32::UI::Input::Pointer::POINTER_PEN_INFO::default();
    if is_pen {
        unsafe {
            windows_sys::Win32::UI::Input::Pointer::GetPointerPenInfo(pointer_id, &mut pen);
        }
    }

    let sample = if is_pen {
        pen_sample(&pen, "数位笔")
    } else {
        let mut mouse = windows_sys::Win32::UI::Input::Pointer::POINTER_INFO::default();
        let ok =
            unsafe { windows_sys::Win32::UI::Input::Pointer::GetPointerInfo(pointer_id, &mut mouse) };
        if ok == 0 {
            return None;
        }
        let (x, y) = screen_to_logical(mouse.ptPixelLocation.x, mouse.ptPixelLocation.y);
        PointerSample {
            logical: (x, y),
            pressure: 1.0,
            pressure_available: false,
            device_has_pressure: false,
            tool: ToolKind::Mouse,
            device_name: "鼠标".to_string(),
            tilt: None,
            shift: shift_down(),
        }
    };

    let history = if is_pen {
        pen_history(pointer_id)
    } else {
        mouse_history(pointer_id)
    };
    Some(PointerReading {
        sample,
        phase,
        pointer_id,
        history,
    })
}

fn pen_sample(
    pen: &windows_sys::Win32::UI::Input::Pointer::POINTER_PEN_INFO,
    device_name: &str,
) -> PointerSample {
    let info = pen.pointerInfo;
    let pressure_available = pen.penMask & PEN_MASK_PRESSURE != 0;
    let pressure = if pressure_available {
        (pen.pressure as f64 / 1024.0).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let tilt = if pen.penMask & (PEN_MASK_TILT_X | PEN_MASK_TILT_Y) != 0 {
        Some((pen.tiltX as f64 / 90.0, pen.tiltY as f64 / 90.0))
    } else {
        None
    };
    let tool = if pen.penFlags & (PEN_FLAG_ERASER | PEN_FLAG_INVERTED) != 0 {
        ToolKind::Eraser
    } else {
        ToolKind::Pen
    };
    let (x, y) = screen_to_logical(info.ptPixelLocation.x, info.ptPixelLocation.y);
    PointerSample {
        logical: (x, y),
        pressure,
        pressure_available,
        device_has_pressure: true,
        tool,
        device_name: device_name.to_string(),
        tilt,
        shift: shift_down(),
    }
}

/// 取一帧内的历史采样点（时间顺序，不含当前点）。
fn pen_history(pointer_id: u32) -> Vec<PointerSample> {
    let mut count = 0u32;
    unsafe {
        windows_sys::Win32::UI::Input::Pointer::GetPointerPenInfoHistory(
            pointer_id,
            &mut count,
            std::ptr::null_mut(),
        );
    }
    if count <= 1 || count > 128 {
        return Vec::new();
    }
    let mut buffer = vec![
        windows_sys::Win32::UI::Input::Pointer::POINTER_PEN_INFO::default();
        count as usize
    ];
    let ok = unsafe {
        windows_sys::Win32::UI::Input::Pointer::GetPointerPenInfoHistory(
            pointer_id,
            &mut count,
            buffer.as_mut_ptr(),
        )
    };
    if ok == 0 {
        return Vec::new();
    }
    buffer.truncate(count as usize);
    // 历史点按“最新在前”返回；反转成时间顺序，并丢掉最后一项（它就是当前点）。
    buffer.reverse();
    buffer
        .into_iter()
        .skip(1)
        .map(|pen| pen_sample(&pen, "数位笔"))
        .collect()
}

/// 鼠标的历史采样点（时间顺序，不含当前点）。
///
/// `WM_POINTERUPDATE` 把一次拖拽合并成很少几条消息；历史点能把中间的移动补回来。
fn mouse_history(pointer_id: u32) -> Vec<PointerSample> {
    let mut count = 0u32;
    unsafe {
        windows_sys::Win32::UI::Input::Pointer::GetPointerInfoHistory(
            pointer_id,
            &mut count,
            std::ptr::null_mut(),
        );
    }
    if count <= 1 || count > 128 {
        return Vec::new();
    }
    let mut buffer = vec![
        windows_sys::Win32::UI::Input::Pointer::POINTER_INFO::default();
        count as usize
    ];
    let ok = unsafe {
        windows_sys::Win32::UI::Input::Pointer::GetPointerInfoHistory(
            pointer_id,
            &mut count,
            buffer.as_mut_ptr(),
        )
    };
    if ok == 0 {
        return Vec::new();
    }
    buffer.truncate(count as usize);
    buffer.reverse();
    buffer
        .into_iter()
        .skip(1)
        .map(|info| {
            let (x, y) = screen_to_logical(info.ptPixelLocation.x, info.ptPixelLocation.y);
            PointerSample {
                logical: (x, y),
                pressure: 1.0,
                pressure_available: false,
                device_has_pressure: false,
                tool: ToolKind::Mouse,
                device_name: "鼠标".to_string(),
                tilt: None,
                shift: shift_down(),
            }
        })
        .collect()
}

/// 一帧处理完后丢掉重复的指针消息，避免同一批点被处理两遍。
pub fn skip_frame(pointer_id: u32) {
    unsafe {
        windows_sys::Win32::UI::Input::Pointer::SkipPointerFrameMessages(pointer_id);
    }
}

pub fn shift_down() -> bool {
    const VK_SHIFT: i32 = 0x10;
    unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState(VK_SHIFT) & 0x8000u16 as i16
            != 0
    }
}
