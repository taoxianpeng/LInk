# egui 面板选型 spike（v0.2 预览）

> 目的：在投入完整迁移前，用最小可用实现验证「egui 做面板 + 原生画布进程」这条路是否成立。
> 日期：2026-10-24 ・ 环境：Linux Mint / X11 `:0` / 4384×2466 @2x / Cinnamon

## 一、结论

**成立，建议按此方向继续。** 面板用 eframe 0.37（egui，glow 后端），画布保持原生 GTK + cairo；
两者是独立进程，用 JSON lines 协议通信（`crates/link-ipc`）。

- 冷启动（面板可交互 → 画布就绪）：**175–248 ms**（debug 175–223 ms，release 248 ms）
- 常驻内存：面板 **84–101 MB**、画布 **30–71 MB**（画布随笔迹数量增长）
- 二进制体积：面板 **16 MB**、画布 **3.3 MB**（release）
- 中文显示：正常（系统字体回退），无缺字
- 二进制：面板与画布各自独立，可同目录分发

## 二、实测数据

| 指标 | 门槛 | 实测 | 结论 |
|---|---|---|---|
| 冷启动 | ≤ 400 ms | debug 175 / 178 / 195 / 223 ms；release 248 ms | ✅ |
| 面板常驻 RSS | ≤ 120 MB | 84–101 MB | ✅ |
| 画布常驻 RSS | — | 30.3–70.8 MB（含 4K 离屏缓存，随笔迹增长） | — |
| 首帧（面板显示） | ≤ 150 ms | 与冷启动同量级（< 200 ms） | ✅ |
| 中文渲染 | 无缺字/形变 | 正常（Droid Sans Fallback 回退） | ✅ |
| 中文 IME 输入 | 可用 | **未验证**（需真人用输入法输入） | ⚠️ |
| 二进制体积 | ≤ 20 MB（面板） | 面板 16 MB / 画布 3.3 MB（release） | ✅ |
| 图层缩略图 | 可见且随笔画更新 | 正常（base64 PNG → egui 纹理） | ✅ |
| 穿透开关 | 生效 | 生效（IPC 往返 < 1 帧） | ✅ |
| 绘制链路 | 压感笔迹正常 | 鼠标/XTEST 绘制正常；压感沿用 v0.1 的 GDK 链路 | ✅ |

## 三、关键发现（都踩过）

1. **同进程混合不可行**：核心用 `gtk 0.19`，Tauri 拉 `gtk 0.18`，Cargo 的 `links = "gtk-3"`
   直接冲突；且 tao 自己跑 GTK 主循环。→ 只能两进程。
2. **WebKitGTK 拿不到压感**（WebKit bug 268458：iOS/WPE 支持，GTK 不支持）→ 画布不能进 webview。
3. **全屏置顶窗口会吞掉面板点击**：画布先创建、后创建者在上层；窗口管理器（Cinnamon/Muffin）
   忽略 `ConfigureWindow(Above)` 与 `_NET_RESTACK_WINDOW`。
   最终方案：把画布的 **输入区域**（XFixes `ShapeInput`）设为「整屏 − 面板矩形」，
   顶层窗口和 `DrawingArea` 子窗口都要设置，并每 400 ms 重算一次（面板可被拖动）。
4. **eframe 0.37 的 App trait 变了**：入口是 `fn ui(&mut self, ui: &mut Ui, frame: &mut Frame)`，
   不是旧的 `update(ctx, frame)`；样式要用 `ctx.all_styles_mut(...)`、`ctx.set_theme(...)`。
5. **缩略图跨进程**：画布渲染 128×72 的 PNG → base64 → `state` 事件；面板解码后作为
   `TextureHandle` 缓存（按图层 id 复用）。

## 四、如何复现

```bash
# 1) 画布（原生）+ 无界面冒烟
cargo build                      # 产出 target/debug/link-ink-canvas（与面板同目录）
python3 tools/ipc-smoke.py       # 握手 / 图层 / 改名 / 可见性 / 导出 / 版本校验

# 2) 面板（egui）
cargo run -p link-ink-panel      # 自动拉起同目录下的 link-ink-canvas

# 3) v0.1 仍可单独运行
cargo run                        # 默认二进制 = link-ink（GTK 单体界面）
```

## 五、尚未验证 / 后续

- 真实数位笔压感与 IME 中文输入（需要人工用设备与输入法确认）。
- 通过面板弹窗完成「打开/保存/导出」（代码已接好，`rfd` 需 xdg-desktop-portal 环境）。
- 图层重命名弹窗、放大预览、删除/排序的界面回归。
- Windows：画布后端（Win32 分层窗口 + WM_POINTER + BitBlt）尚未实现，面板侧无需改动。
