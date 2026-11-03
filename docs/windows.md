# Windows 平台适配说明

> 状态：**渲染、输入、截图链路均已打通**；鼠标走全局低级钩子（参考 geovens/gink），
> 笔走 `WM_POINTER`。详见「三、Windows 画布实现」。

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

**三个必须同时满足的条件**（少一个就完全看不见，且都不报错）：

1. **`WS_EX_LAYERED` 必须在 `CreateWindowExW` 时就带上**。
   窗口创建后再 `SetWindowLong` 补这个位，`UpdateLayeredWindowIndirect` 会返回
   `87 / ERROR_INVALID_PARAMETER`；如果调用方用 `let _ =` 忽略错误，
   表现就是"提交成功、屏幕永远空白"。
2. **首帧必须在 `ShowWindow` 之前提交**（带 `pptDst` / `psize`）。
   先显示再提交，DWM 会缓存"空窗口"状态，后续所有更新静默失效。
3. **底图 alpha 不能是 0 或 1**。DWM 会整层跳过近乎全透明的分层窗口。
   现在用 **8**：肉眼看几乎无感，但足以参与合成与命中测试。

其余细节：
- 样式：`WS_POPUP` + `WS_EX_LAYERED | WS_EX_TOOLWINDOW`
- 位图：`CreateDIBSection`，32bpp、`biHeight` 取**负值**（top-down）、预乘 BGRA
- 提交：`UpdateLayeredWindow`（非 Indirect）+ `ULW_ALPHA` + `AC_SRC_OVER/AC_SRC_ALPHA`，
  `hdcDst` 传**屏幕 DC**（与 gink 的 `UpdateFormDisplay` 一致）
- 全屏整窗提交（约 2–3 ms @4K）：`prcDirty` 会把历史笔迹裁掉

### 输入（`src/platform/win/input.rs` + `mouse_hook.rs`）

**鼠标和笔走两条完全不同的通道**：

| | 通道 | 原因 |
|---|---|---|
| 数位笔 | `WM_POINTER*` + `GetPointerPenInfo` | 只有这里能拿到压感/倾斜/橡皮擦 |
| 鼠标 | `WH_MOUSE_LL` 全局低级钩子 | 见下 |

鼠标不用窗口消息的原因：
- 覆盖层是分层窗口，命中测试**按像素 alpha** 判断，近乎全透明的底图会让
  鼠标事件直接穿透到桌面；
- 即使能收到，`WM_POINTER` 对鼠标也会把整次拖拽合并成极少几条消息
  （实测一次拖拽只有 Down/Move/Up 三条），连续笔迹会退化成折线。

低级钩子由系统在所有鼠标事件上回调，与焦点、命中测试、窗口样式都无关，
采样密度等于真实事件密度（gink 用 Microsoft.Ink 的 `InkCollector` 达到同样效果）。

笔的消息要点：
- `penMask & PEN_MASK_PRESSURE` 为真才认为压感有效（否则是"没有这个轴"，不是 0）
- `PEN_FLAG_INVERTED | PEN_FLAG_ERASER` → 橡皮擦
- `GetPointerPenInfoHistory` 补齐一帧内的历史点，避免快速书写折线
- 用 `GetPointerType` 判断是不是笔：`GetPointerPenInfo` 对鼠标也会成功

消息循环：用 `PeekMessage` + 短睡眠，而不是阻塞式 `GetMessage`。
低级钩子的回调依赖线程消息泵，阻塞在 `GetMessage` 里会大量丢事件。

穿透：`WM_NCHITTEST` 返回 `HTTRANSPARENT`（含"面板矩形让给面板"）。

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

## 五、显卡 / 渲染后端

面板用 eframe 的 **wgpu** 后端（glow/WGL 在部分 NVIDIA 驱动上只画得出底色）。

Windows 上**必须排除 Vulkan**：

```rust
create_new.instance_descriptor.backends =
    wgpu::Backends::from_env().unwrap_or(wgpu::Backends::DX12 | wgpu::Backends::GL);
```

原因：Intel HD 530（及同代驱动）在 `adapter.request_device()` 内部直接踩空指针，
进程 `STATUS_ACCESS_VIOLATION`。它**返回不了 `Err`**，所以任何运行时回退都兜不住 ——
唯一办法是一开始就别把 Vulkan 列进候选。

主用 DX12、GL 兜底：老机器上 DX12 可能只剩 WARP 软件光栅，此时 wgpu 按设备类型
会优先挑 GL 而不是 CPU 适配器。`WGPU_BACKEND` 环境变量仍可覆盖。

崩溃时的日志特征（`RUST_LOG=debug`）：

```
wgpu_core::instance] Request adapter result ... backend: Vulkan
wgpu_hal::vulkan::adapter] Supported extensions: [...]   ← 最后一行
→ STATUS_ACCESS_VIOLATION
```

## 六、已知限制

- 只覆盖主显示器（与 Linux 版一致）
- 面板是不透明窗口（Windows 的 WGL/合成 alpha 不稳定）
- `BitBlt` 抓不到独占全屏/受保护内容；HDR 屏可能偏色
- 鼠标钩子是全局的：若其他程序也装了 `WH_MOUSE_LL`，可能互相影响
- 合成输入（`SetCursorPos` / `mouse_event`）在本机 DPI=1.75 下采样不稳定，
  自动化测试无法可靠复现绘制；真实设备请手动验收

## 七、日志（反馈问题用）

**debug 构建**会把面板和画布的日志落到文件里，用户直接把文件发出来即可定位：

| 文件 | 内容 |
|---|---|
| `%APPDATA%\link-ink\link-panel.log` | 面板：wgpu 适配器、字体、握手、冷启动耗时 |
| `%APPDATA%\link-ink\link-canvas.log` | 画布：主屏信息、鼠标钩子、覆盖层、提交错误 |

- release 构建**不写文件**（避免拖慢渲染、文件无限增长）
- 每次启动覆盖上一次，不保留历史
- 面板「设置」窗口里有「打开日志目录」按钮

实现见 `src/logfile.rs`。两个关键决定：

1. **两个进程各写各的文件**。曾经试过写同一个文件，但面板和画布各持独立文件偏移，
   Windows 的 `append` 不保证跨进程原子追加，实测出现过整行被覆盖（日志里出现
   `_ink::platform::win::mouse_hook]` 这种被砍掉前缀的行）和时间戳乱序。
2. **文件初始化失败绝不影响启动**，只回退到控制台。

## 八、工具链

仓库根目录的 `rust-toolchain.toml` 把工具链钉在 **1.99.1**
（eframe/egui 0.37 要求 rustc >= 1.95）。不带这个文件时，
协作者默认 stable 若低于 1.95，`cargo build` 会因 MSRV 检查失败 ——
而已构建过的 `target/` 会掩盖这个问题，直到一次 `cargo clean`。

## 九、手动验收清单（Linux 侧必做）

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
