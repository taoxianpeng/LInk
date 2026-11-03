//! Windows 画布进程：Win32 分层窗口 + `WM_POINTER` 压感 + tiny-skia 渲染。
//!
//! 结构与 GTK 版一致：同一份 [`CanvasApp`] 控制器，窗口/截图/穿透由这里注入。
//!
//! 消息循环要点：
//! - IPC 读线程收到命令后 `PostMessageW(WM_APP_COMMAND)` 唤醒主线程（替代 GTK 的 8 ms 轮询）；
//! - 每帧先把 tiny-skia 画到整张预乘 RGBA 缓冲，再把脏矩形转 BGRA 上传给分层窗口；
//! - `WM_NCHITTEST` 处理穿透与面板矩形（见 `platform::win::overlay`）。

use std::cell::RefCell;
use std::io::{self, BufReader};
use std::sync::mpsc;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, PeekMessageW, PostMessageW, PostQuitMessage, SetTimer,
    ShowWindow, TranslateMessage, MSG, PM_REMOVE, SW_HIDE, SW_SHOWNOACTIVATE, WM_APP, WM_CLOSE,
    WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WM_TIMER,
};

use link_ipc::{read_command, write_event, Command, Event};

use link_ink::canvas_app::{CanvasApp, CanvasHost};
use link_ink::damage::{merge_scope, union_rect, DamageRect, DrawScope};
use link_ink::input::InputEvent;
use link_ink::paint::{new_surface, RectF, Surface};
use link_ink::platform::win::input::{self, PointerPhase};
use link_ink::platform::win::mouse_hook::{self, MouseHook, MousePhase as HookPhase};
use link_ink::platform::win::overlay::{self, lparam_point};
use link_ink::platform::win::window::{self, DirtyRect, LayeredBitmap};
use link_ink::platform::win::WinPlatform;
use link_ink::platform::{CaptureImage, MonitorInfo, PlatformError};

/// 命令到达消息（IPC 读线程 → 窗口线程）。
const WM_APP_COMMAND: u32 = WM_APP + 1;
/// 面板矩形刷新定时器（与 Linux 的 400 ms 一致）。
const TIMER_PANEL: usize = 1;
/// 鼠标钩子队列的消费定时器。
const TIMER_PUMP: usize = 2;
const PANEL_INTERVAL_MS: u32 = 400;
/// 隐藏覆盖层后等待 DWM 出帧的时间（约两帧 @60Hz）。
const CAPTURE_DELAY_MS: u64 = 150;

struct CanvasState {
    hwnd: HWND,
    app: CanvasApp,
    host: WinHost,
    /// tiny-skia 的预乘 RGBA 缓冲（与 DIB 同尺寸）。
    frame: Box<dyn Surface>,
    bitmap: LayeredBitmap,
    origin: (i32, i32),
    /// 物理像素 / 逻辑像素（鼠标钩子给的是屏幕物理坐标）。
    scale: f64,
    /// 待绘制的范围（多个输入事件合并后取并集）。
    pending: Option<DrawScope>,
    /// 上一帧画过的脏矩形，用于把两帧的矩形并起来。
    last_damage: Option<DamageRect>,
    /// IPC 命令队列。
    rx: mpsc::Receiver<Command>,
    quit: bool,
}

struct WinHost {
    hwnd: HWND,
}

impl WinHost {
    fn show(&self, visible: bool) {
        unsafe {
            ShowWindow(self.hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }

    /// 隐藏窗口后仍要排空消息，否则恢复时会看到积压的旧输入。
    fn pump_messages(&self) {
        let mut msg = MSG::default();
        loop {
            let has = unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) };
            if has == 0 || msg.message == WM_PAINT {
                break;
            }
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        unsafe {
            windows_sys::Win32::Graphics::Dwm::DwmFlush();
        }
    }
}

impl CanvasHost for WinHost {
    fn set_click_through(&mut self, enabled: bool) -> Result<(), PlatformError> {
        overlay::set_passthrough(enabled);
        Ok(())
    }

    fn capture_primary_hidden(&mut self) -> Result<CaptureImage, PlatformError> {
        let platform = WinPlatform::new()?;
        self.show(false);
        self.pump_messages();
        std::thread::sleep(std::time::Duration::from_millis(CAPTURE_DELAY_MS));
        let result = platform.capture_primary();
        self.show(true);
        self.pump_messages();
        result
    }
}

thread_local! {
    static MOUSE_RX: RefCell<Option<mpsc::Receiver<mouse_hook::MouseEvent>>> =
        const { RefCell::new(None) };
}

thread_local! {
    static STATE: RefCell<Option<Box<CanvasState>>> = const { RefCell::new(None) };
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let result = match msg {
        // WM_NCCREATE 必须交给 DefWindowProcW：它负责初始化窗口内部状态并返回 TRUE。
        // 直接返回 1 会让分层窗口的合成状态不完整。
        windows_sys::Win32::UI::WindowsAndMessaging::WM_NCCREATE => {
            window::default_wndproc(hwnd, msg, wparam, lparam)
        }
        WM_NCHITTEST => {
            let (x, y) = overlay::lparam_point(lparam);
            overlay::hit_test(x, y)
        }
        input::WM_POINTER_DOWN => {
            handle_pointer(wparam, PointerPhase::Down);
            0
        }
        input::WM_POINTER_UPDATE => {
            handle_pointer(wparam, PointerPhase::Move);
            0
        }
        input::WM_POINTER_UP => {
            handle_pointer(wparam, PointerPhase::Up);
            0
        }
        input::WM_POINTER_ENTER => {
            handle_pointer(wparam, PointerPhase::Enter);
            0
        }
        input::WM_POINTER_LEAVE => {
            handle_pointer(wparam, PointerPhase::Leave);
            0
        }
        input::WM_POINTER_CAPTURE_CHANGED => {
            push_input(InputEvent::Cancel);
            0
        }
        // 鼠标走传统消息：`WM_POINTER` 把一次拖拽合并成极少数几条，
        // 画出来只有起止两点。按下时 SetCapture 保证移动事件持续送到本窗口
        // （窗口是 WS_EX_NOACTIVATE，不捕获就收不到）。
        WM_LBUTTONDOWN => {
            unsafe {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::SetCapture(hwnd);
            }
            handle_mouse(lparam, MousePhase::Down);
            0
        }
        WM_MOUSEMOVE => {
            handle_mouse(lparam, MousePhase::Move);
            0
        }
        WM_LBUTTONUP => {
            unsafe {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
            }
            handle_mouse(lparam, MousePhase::Up);
            0
        }
        WM_APP_COMMAND => {
            handle_commands();
            0
        }
        WM_TIMER if wparam == TIMER_PUMP => {
            pump_mouse_hook();
            0
        }
        WM_TIMER if wparam == TIMER_PANEL => {
            overlay::refresh_panel_rect();
            0
        }
        WM_PAINT => {
            // 分层窗口的内容由 UpdateLayeredWindowIndirect 提交，这里只需 validate。
            unsafe {
                windows_sys::Win32::Graphics::Gdi::ValidateRect(hwnd, std::ptr::null());
            }
            0
        }
        WM_CLOSE | WM_DESTROY => {
            unsafe {
                PostQuitMessage(0);
            }
            0
        }
        _ => window::default_wndproc(hwnd, msg, wparam, lparam),
    };
    let _ = hwnd;
    result
}

fn handle_pointer(wparam: WPARAM, phase: PointerPhase) {
    let Some(reading) = input::read_pointer(wparam, phase) else {
        return;
    };
    // 一帧内的历史点补发成 Move，避免快速书写出现折线。
    if matches!(reading.phase, PointerPhase::Down | PointerPhase::Move | PointerPhase::Enter) {
        for sample in &reading.history {
            push_input(InputEvent::Move(sample.clone()));
        }
    }
    let event = match reading.phase {
        PointerPhase::Down => InputEvent::Down(reading.sample),
        PointerPhase::Move | PointerPhase::Enter => InputEvent::Move(reading.sample),
        PointerPhase::Up => InputEvent::Up(reading.sample),
        PointerPhase::Leave | PointerPhase::Cancel => InputEvent::ProximityOut,
    };
    push_input(event);
}

#[derive(Debug, Clone, Copy)]
enum MousePhase {
    Down,
    Move,
    Up,
}

/// 传统鼠标消息 → `PointerSample`。
///
/// PerMonitorV2 下 `lParam` 是**客户区物理像素**（窗口本身就是 3840×2160 物理），
/// 而 `CanvasApp.view` 是逻辑视图，所以要除以显示器缩放后才能落到正确的页面位置。
fn handle_mouse(lparam: LPARAM, phase: MousePhase) {
    let (x, y) = lparam_point(lparam);
    let (lx, ly) = input::physical_to_logical(x, y);
    let sample = link_ink::input::PointerSample {
        logical: (lx, ly),
        pressure: 1.0,
        pressure_available: false,
        device_has_pressure: false,
        tool: link_ink::input::ToolKind::Mouse,
        device_name: "鼠标".to_string(),
        tilt: None,
        shift: input::shift_down(),
    };
    let event = match phase {
        MousePhase::Down => InputEvent::Down(sample),
        MousePhase::Move => InputEvent::Move(sample),
        MousePhase::Up => InputEvent::Up(sample),
    };
    push_input(event);
}

/// 消费鼠标钩子队列（由 8 ms 定时器驱动）。
///
/// 鼠标为什么走钩子而不是窗口消息：覆盖层是分层窗口，命中测试按像素 alpha 判断，
/// 近乎全透明的底图会让鼠标消息直接穿透到桌面；即使能收到，
/// `WM_POINTER` 对鼠标也会把整次拖拽合并成极少几条消息。
/// 低级钩子由系统在所有鼠标事件上回调，与焦点/命中/窗口样式无关。
fn pump_mouse_hook() {
    MOUSE_RX.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let Some(rx) = borrow.as_mut() else {
            return;
        };
        while let Ok(event) = rx.try_recv() {
            let (origin, scale) = STATE.with(|state| {
                state
                    .borrow()
                    .as_ref()
                    .map(|state| (state.origin, state.scale))
                    .unwrap_or(((0, 0), 1.0))
            });
            let sample = mouse_hook::to_sample(event, origin, scale);
            let input = match event.phase {
                HookPhase::Down => InputEvent::Down(sample),
                HookPhase::Move => InputEvent::Move(sample),
                HookPhase::Up => InputEvent::Up(sample),
            };
            push_input(input);
        }
    });
}

fn push_input(event: InputEvent) {
    STATE.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let Some(state) = borrow.as_mut() else {
            return;
        };
        let events = state.app.on_input(event);
        emit(events);
        request_redraw(state);
    });
}

/// 根据控制器状态决定这一帧重绘多大范围。
fn request_redraw(state: &mut CanvasState) {
    let needs_redraw = state.app.take_redraw();
    let force_full = state.app.app_mut().take_force_full_redraw();
    if !needs_redraw && !force_full {
        return;
    }
    let full = force_full || state.app.app().is_erasing();
    if full {
        state.last_damage = None;
        state.pending = Some(DrawScope::Full);
        render(state);
        return;
    }
    let damage = state.app.damage_to_device(state.app.app().interaction_damage_pt());
    let previous = std::mem::replace(&mut state.last_damage, damage);
    let Some(rect) = union_rect(damage, previous) else {
        state.pending = Some(DrawScope::Full);
        render(state);
        return;
    };
    let scope = DrawScope::Partial((rect.0 - 2.0, rect.1 - 2.0, rect.2 + 2.0, rect.3 + 2.0));
    state.pending = Some(merge_scope(state.pending, scope));
    render(state);
}

fn handle_commands() {
    let mut events = Vec::new();
    let mut quit = false;
    STATE.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let Some(state) = borrow.as_mut() else {
            return;
        };
        while let Ok(command) = state.rx.try_recv() {
            if matches!(command, Command::Quit) {
                quit = true;
                break;
            }
            log::debug!("画布收到命令: {command:?}");
            events.extend(state.app.handle_command(command, &mut state.host));
            let _ = state.app.take_redraw();
            let _ = state.app.app_mut().take_force_full_redraw();
            state.last_damage = None;
            state.pending = Some(DrawScope::Full);
        }
        if !events.is_empty() {
            state.last_damage = None;
            state.pending = Some(DrawScope::Full);
        }
    });
    emit(events);
    if quit {
        unsafe {
            PostQuitMessage(0);
        }
        return;
    }
    let full = STATE.with(|slot| {
        let mut borrow = slot.borrow_mut();
        let Some(state) = borrow.as_mut() else {
            return false;
        };
        if state.pending == Some(DrawScope::Full) {
            render(state);
            true
        } else {
            false
        }
    });
    let _ = full;
}

fn render(state: &mut CanvasState) {
    let scope = state.pending.take();
    let clip = match scope {
        Some(DrawScope::Partial((x0, y0, x1, y1))) => RectF::from_bounds(x0, y0, x1, y1),
        Some(DrawScope::Full) | None => state.app.full_clip(),
    };
    let damage = match scope {
        Some(DrawScope::Partial(rect)) => Some(rect),
        _ => None,
    };

    let frame = &mut state.frame;
    let app = &mut state.app;
    frame.with_canvas(&mut |canvas| {
        app.draw(canvas, damage, clip);
    });

    let width = state.bitmap.width();
    let height = state.bitmap.height();
    // 一律整窗提交。
    //
    // `prcDirty` 只限制“从源 DC 拷贝哪一块”，而画布上已有的笔迹分布在全屏，
    // 局部脏矩形会把历史内容裁掉（表现为新笔迹只有一部分可见）。
    // 4K 整窗上传在本机约 2–3 ms，仍在帧预算内。
    let dirty: Option<DirtyRect> = None;
    let _ = width;
    let _ = height;
    let mut rgba = state.frame.read_rgba();
    // 透明像素的 alpha 抬到 BACKDROP_ALPHA，理由见该常量的注释。
    for pixel in rgba.chunks_exact_mut(4) {
        if pixel[3] == 0 {
            pixel[3] = BACKDROP_ALPHA;
        }
    }
    state.bitmap.upload_premultiplied_rgba(&rgba, dirty);
    if let Err(error) = state
        .bitmap
        .update_window(state.hwnd, state.origin, dirty, false)
    {
        log::error!("提交分层窗口失败: {error}");
    }
}

/// 全屏底图的 alpha。
///
/// - 0/1：DWM 会跳过整层合成，笔迹永远刷不出来（实测）；
/// - 8：深色/浅色背景上都几乎看不出，同时能稳定参与合成与命中测试。
const BACKDROP_ALPHA: u8 = 8;

fn near_invisible_backdrop(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[3] = BACKDROP_ALPHA;
    }
    rgba
}

fn emit(events: Vec<Event>) {
    if events.is_empty() {
        return;
    }
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    for event in events {
        if let Err(error) = write_event(&mut lock, &event) {
            log::error!("写 IPC 事件失败: {error}");
            break;
        }
    }
}

pub fn run() -> i32 {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let platform = match WinPlatform::new() {
        Ok(platform) => platform,
        Err(error) => {
            log::error!("画布启动失败: {error}");
            emit(vec![Event::Error {
                message: format!("画布启动失败：{error}"),
            }]);
            return 1;
        }
    };
    match start(*platform.monitor()) {
        Ok(()) => 0,
        Err(error) => {
            log::error!("画布启动失败: {error}");
            emit(vec![Event::Error {
                message: format!("画布启动失败：{error}"),
            }]);
            1
        }
    }
}

fn start(monitor: MonitorInfo) -> Result<(), PlatformError> {
    // 鼠标走全局低级钩子（见 `pump_mouse_hook` 的说明）。
    // 钩子句柄必须活到进程结束（Drop 时 UnhookWindowsHookEx）。
    let (_mouse_hook, mouse_rx) = match MouseHook::install() {
        Some((hook, rx)) => (Some(hook), Some(rx)),
        None => (None, None),
    };
    window::register_class(Some(wndproc))?;
    let hwnd = window::create_overlay_window(monitor.physical)?;
    overlay::refresh_panel_rect();
    overlay::set_passthrough(false);
    input::set_screen_mapping((monitor.physical.x, monitor.physical.y), monitor.scale);
    input::enable_mouse_in_pointer();

    let bitmap = LayeredBitmap::new(monitor.physical.width, monitor.physical.height)?;
    let frame = new_surface(monitor.physical.width, monitor.physical.height)
        .map_err(|error| PlatformError::Capture(error.to_string()))?;

    let (tx, rx) = mpsc::channel::<Command>();
    let hwnd_value = hwnd as isize;
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        loop {
            match read_command(&mut reader) {
                Ok(Some(command)) => {
                    let quit = matches!(command, Command::Quit);
                    if tx.send(command).is_err() {
                        break;
                    }
                    unsafe {
                        PostMessageW(hwnd_value as HWND, WM_APP_COMMAND, 0, 0);
                    }
                    if quit {
                        break;
                    }
                }
                Ok(None) => {
                    log::info!("面板已关闭（stdin EOF），画布退出");
                    break;
                }
                Err(error) => log::warn!("IPC 解析失败: {error}"),
            }
        }
        // EOF：让窗口线程退出。
        unsafe {
            PostMessageW(hwnd_value as HWND, WM_CLOSE, 0, 0);
        }
    });

    let state = Box::new(CanvasState {
        hwnd,
        app: CanvasApp::new(monitor),
        host: WinHost { hwnd },
        frame,
        bitmap,
        origin: (monitor.physical.x, monitor.physical.y),
        scale: monitor.scale,
        pending: Some(DrawScope::Full),
        last_damage: None,
        quit: false,
        rx,
    });
    STATE.with(|slot| *slot.borrow_mut() = Some(state));
    MOUSE_RX.with(|slot| *slot.borrow_mut() = mouse_rx);

    unsafe {
        SetTimer(hwnd, TIMER_PANEL, PANEL_INTERVAL_MS, None);
    }

    // 首帧：整屏提交一次。
    //
    // 关键：分层窗口的命中测试是按像素 alpha 做的——整窗全透明时系统会认为
    // 窗口"不存在"，指针事件直接穿到桌面。所以底图要有一层肉眼几乎看不见、
    // 但非零的 alpha（`BACKDROP_ALPHA`）；真正的"穿透"由
    // `WM_NCHITTEST` 返回 `HTTRANSPARENT` 实现。
    STATE.with(|slot| {
        if let Some(state) = slot.borrow_mut().as_mut() {
            let rgba = near_invisible_backdrop(state.bitmap.width(), state.bitmap.height());
            state.bitmap.upload_premultiplied_rgba(&rgba, None);
            let _ = state.bitmap.update_window(state.hwnd, state.origin, None, false);
        }
    });
    // 首帧就位后再显示窗口（顺序错了 DWM 会一直缓存空窗口）。
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        windows_sys::Win32::UI::WindowsAndMessaging::SetWindowPos(
            hwnd,
            windows_sys::Win32::UI::WindowsAndMessaging::HWND_TOPMOST,
            0,
            0,
            0,
            0,
            windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                | windows_sys::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE
                | windows_sys::Win32::UI::WindowsAndMessaging::SWP_SHOWWINDOW,
        );
    }
    log::info!("覆盖层已显示（全屏 alpha={BACKDROP_ALPHA} 底图）");


    // 用 PeekMessage 而不是阻塞式 GetMessage：低级鼠标钩子的回调依赖线程消息泵，
    // 阻塞在 GetMessage 时钩子事件会大量丢失。
    let mut msg = MSG::default();
    loop {
        let got = unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) };
        if got == 0 {
            pump_mouse_hook();
            std::thread::sleep(std::time::Duration::from_millis(2));
            continue;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let quit = STATE.with(|slot| {
            slot.borrow_mut()
                .as_ref()
                .map(|state| state.quit)
                .unwrap_or(true)
        });
        if quit {
            break;
        }
    }

    STATE.with(|slot| {
        slot.borrow_mut().take();
    });
    window::destroy_window(hwnd);
    log::info!("画布退出");
    Ok(())
}
