# Windows 平台适配说明

> 状态：**架构与渲染完成，覆盖层的屏幕合成未打通**（详见文末「已知问题」）。

## 一、与 Linux 后端的关系

两个平台共用同一份控制器与协议，只有「窗口 / 输入 / 截图」和「光栅化」两层不同：

```
                      ┌──────────────────────────┐
                      │  link-ipc（JSON lines）  │
                      └────────────┬─────────────┘
                                   │
                     ┌─────────────▼─────────────┐
                     │  CanvasApp（无 GUI 控制器）│  ← 两平台共用
                     │  文档 / 缓存 / 视图 / 手势 │
                     └─────────────┬─────────────┘
                                   │ Canvas / Surface trait
                 ┌─────────────────┴─────────────────┐
        Linux    │                                   │  Windows
   ┌─────────────▼────────────┐         ┌────────────▼─────────────┐
   │ paint/cairo_backend.rs   │         │ paint/skia_backend.rs    │
   │ GTK 窗口 + XFixes 输入区域│         │ 分层窗口 + WM_POINTER    │
   └──────────────────────────┘         └──────────────────────────┘
```

`crates/link-ipc` 的协议、`CanvasApp`、`App`、模型/渲染几何全部**零改动**；
`PROTOCOL_VERSION` 保持 3。

## 二、渲染抽象（`src/paint/`）

cairo 的几项能力在 tiny-skia 里没有对应物，抽象层自己补上：

| cairo | 抽象层做法 |
|---|---|
| `set_device_scale` | 不用：调用方统一用 `ViewTransform::to_physical_pixels` 换算到设备像素 |
| `save`/`restore` 状态栈 | `SkiaCanvas` 内部维护 `saved: Vec<State>` |
| `clip()` | 记录裁剪矩形，绘制时与目标求交；必要时建临时位图 |
| `push_group`/`pop_group_to_source` | 按 `bounds` 建离屏 pixmap，组内坐标平移 `-bounds.origin`，回贴用 `BlendMode::Source` |
| `stroke_preserve`/`fill_preserve` | 保留 `Path2`，连续调用 `stroke_path`/`fill_path` |
| `new_sub_path` + `arc` | `PathBuilder2::circle`（独立子路径，不连接当前点） |

**关键不变量**：小区域原子合成的最后一步必须是 `Source` 覆盖（不是 `Over`），
否则形状预览会留残影 —— 对应 `tests/render_cache.rs::shape_preview_does_not_leave_ghosts_between_frames`。

## 三、Windows 画布实现

### 窗口（`src/platform/win/window.rs`）

- 样式：`WS_POPUP` + `WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`
- 位图：`CreateDIBSection`，32bpp、`biHeight` 取**负值**（top-down）、预乘 BGRA
- 提交：`UpdateLayeredWindowIndirect` + `ULW_ALPHA` + `AC_SRC_OVER/AC_SRC_ALPHA`
- **顺序**：`CreateWindowExW` → 提交首帧（带 `pptDst`/`psize`）→ `ShowWindow` → `SetWindowPos`
  （先显示再提交会让 DWM 缓存空窗口状态）

### 输入（`src/platform/win/input.rs`）

- 笔：`WM_POINTER*` + `GetPointerPenInfo`
  - `penMask & PEN_MASK_PRESSURE` 为真才认为压感有效（否则是"没有这个轴"，不是 0）
  - `PEN_FLAG_INVERTED | PEN_FLAG_ERASER` → 橡皮擦
  - `GetPointerPenInfoHistory` 补齐一帧内的历史点，避免快速书写折线
- 鼠标：`WM_POINTER` 会把一次拖拽合并成很少几条消息，且 `lParam` 是**客户区物理像素**
  （PerMonitorV2 下），需要除以 `scale` 才能落到逻辑坐标系
- 命中测试：`WM_NCHITTEST` 返回 `HTTRANSPARENT` 实现穿透（含"面板矩形让给面板"）

### 截图（`src/platform/win/capture.rs`）

`GetDC(NULL)` + 兼容 DC + `BitBlt(SRCCOPY | CAPTUREBLT)`，`GetDIBits` 转 straight RGBA。
`CAPTUREBLT` 失败时退回 `SRCCOPY`。

## 四、面板

- **渲染后端改用 wgpu**：实测 eframe 0.37 + glow(WGL) 在本机 NVIDIA 610.88 上只画得出底色，
  控件内容完全不可见（最小复现程序也一样）；wgpu 正常。
- 中文字体：`%WINDIR%\Fonts\msyh.ttc`（egui 0.37 的 skrifa 支持 TTC）
- 置顶：`SetWindowPos(HWND_TOPMOST, SWP_NOACTIVATE)`（两个 topmost 窗口不会自动重排）
- 退出：先 `client.take()`（kill + wait 画布）再 `ViewportCommand::Close`，
  不用 `std::process::exit`（那会跳过 `Drop`，画布变孤儿进程）
- 背景：不透明 `PANEL_BG`（WGL/合成 alpha 在 Windows 上不可靠）

## 五、已知问题

**覆盖层的运行时刷新不生效**。具体现象与已验证的事实：

| 环节 | 状态 |
|---|---|
| 输入事件到达画布 | ✅ 63 条 Down/Move/Up |
| 笔迹提交进文档 | ✅ 笔迹数 0 → 1 |
| 渲染到帧（`read_rgba`） | ✅ 帧内有 13909 个笔迹像素，坐标正确 |
| 写入 DIB `bits` | ✅ 写入后回读有 13909 个笔迹像素 |
| `UpdateLayeredWindowIndirect` | ✅ 返回 1（成功） |
| 窗口尺寸/位置 | ✅ 3840×2160 @ (0,0)，TOPMOST |
| **屏幕实际显示** | ❌ 看不到 |

对照组：**同样的调用序列在独立的最小程序里完全正常**（纯蓝 3840×2160 全屏窗口可见，
`UpdateLayeredWindow` / `UpdateLayeredWindowIndirect` 均有效，加不加
`TOOLWINDOW|NOACTIVATE`、有没有 DPI 感知、自定义 wndproc 都不影响）。

尚未排除的方向：
- 画布进程同时有面板子进程、wgpu 面板、以及 `SetTimer`/`WM_APP` 消息循环的交互
- 需要独立进程/干净会话复现，或用 DWM 调试工具（PresentMon / DWM 日志）确认合成是否发生

**建议下一步**：把 `src/canvas/win_main.rs` 的窗口装配拆成一个可独立运行的最小可执行文件
（不带 IPC、不带渲染，只提交纯色位图），在**同一台机器**上跑通后再逐步加回图层，
以此定位是哪个组件破坏了合成。

## 六、手动验收清单（Linux 侧必做）

由于本机没有 GTK 开发库，Linux 后端只做了逐调用 1:1 映射，未编译验证。合并前请执行：

```bash
cargo build --workspace
cargo test --workspace
python3 tools/ipc-smoke.py target/debug/link-ink-canvas
cargo run --release
```

重点回归：
- 长笔迹书写不闪（原子合成）
- 拖动矩形/椭圆无残影
- 非顶层书写时草稿被上层盖住
- 导出白色背景 / 当前屏幕
- 面板拖动后仍可点击
