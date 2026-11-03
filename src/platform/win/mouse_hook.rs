//! 低级鼠标钩子（`WH_MOUSE_LL`）：全局接收鼠标按下/移动/抬起。
//!
//! 为什么不用窗口消息：
//! - 覆盖层是 `WS_EX_LAYERED` 窗口，命中测试按像素 alpha 做，
//!   近乎全透明的底图会让鼠标事件直接穿透到桌面；
//! - 即使窗口能收到，`WM_POINTER` 对鼠标会把整次拖拽合并成极少几条消息，
//!   连续笔迹会退化成折线。
//!
//! 低级钩子由系统在**所有**鼠标事件上回调，与焦点、命中测试、窗口样式都无关，
//! 采样密度等于真实事件密度（这也是 gink 一类标注工具依赖的输入通道）。
//! 笔（有压感）仍然走 `WM_POINTER`，那里才有压感轴和历史点。

use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::sync::OnceLock;

use crate::input::{PointerSample, ToolKind};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, MSLLHOOKSTRUCT, WH_MOUSE_LL,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};

/// 钩子回调里只能做极轻的事，所以只把事件塞进通道，由画布主线程消费。
static SENDER: OnceLock<Mutex<Sender<MouseEvent>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MousePhase {
    Down,
    Move,
    Up,
}

#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    pub x: i32,
    pub y: i32,
    pub phase: MousePhase,
    pub shift: bool,
}

pub struct MouseHook {
    hook: HHOOK,
}

impl MouseHook {
    /// 安装全局鼠标钩子。返回的接收端由主线程轮询。
    pub fn install() -> Option<(Self, mpsc::Receiver<MouseEvent>)> {
        let (tx, rx) = mpsc::channel();
        SENDER.set(Mutex::new(tx)).ok()?;

        let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), std::ptr::null_mut(), 0) };
        if hook.is_null() {
            log::warn!(
                "安装 WH_MOUSE_LL 失败：{}（鼠标输入不可用）",
                std::io::Error::last_os_error()
            );
            return None;
        }
        log::info!("已安装全局鼠标钩子");
        Some((Self { hook }, rx))
    }
}

impl Drop for MouseHook {
    fn drop(&mut self) {
        if !self.hook.is_null() {
            unsafe {
                UnhookWindowsHookEx(self.hook);
            }
        }
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = &*(lparam as *const MSLLHOOKSTRUCT);
        let phase = match wparam as u32 {
            WM_MOUSEMOVE => Some(MousePhase::Move),
            WM_LBUTTONDOWN => Some(MousePhase::Down),
            WM_LBUTTONUP => Some(MousePhase::Up),
            _ => None,
        };
        if let Some(phase) = phase {
            if let Some(sender) = SENDER.get() {
                if let Ok(sender) = sender.lock() {
                    let _ = sender.send(MouseEvent {
                        x: info.pt.x,
                        y: info.pt.y,
                        phase,
                        shift: shift_down(),
                    });
                }
            }
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

fn shift_down() -> bool {
    const VK_SHIFT: i32 = 0x10;
    unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState(VK_SHIFT) & 0x8000u16 as i16
            != 0
    }
}

/// 低级钩子给的是**屏幕物理像素**；画布逻辑坐标 = 屏幕坐标 - 主屏原点。
pub fn to_sample(event: MouseEvent, origin: (i32, i32), scale: f64) -> PointerSample {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    PointerSample {
        logical: (
            (event.x - origin.0) as f64 / scale,
            (event.y - origin.1) as f64 / scale,
        ),
        pressure: 1.0,
        pressure_available: false,
        device_has_pressure: false,
        tool: ToolKind::Mouse,
        device_name: "鼠标".to_string(),
        tilt: None,
        shift: event.shift,
    }
}
