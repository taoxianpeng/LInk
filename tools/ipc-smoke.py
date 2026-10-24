#!/usr/bin/env python3
"""画布进程的 IPC 冒烟测试：不需要 UI，直接和 link-ink-canvas 对话。

用法：
    python3 tools/ipc-smoke.py [目标/二进制路径]
"""
import json
import os
import subprocess
import sys
import time

BINARY = sys.argv[1] if len(sys.argv) > 1 else "target/debug/link-ink-canvas"
PROTO = 1


class Canvas:
    def __init__(self, path):
        self.proc = subprocess.Popen(
            [path],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )

    def send(self, **payload):
        self.proc.stdin.write(json.dumps(payload, ensure_ascii=False) + "\n")
        self.proc.stdin.flush()

    def read(self, timeout=5.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                time.sleep(0.01)
                continue
            line = line.strip()
            if line:
                return json.loads(line)
        raise TimeoutError("等待画布事件超时")

    def read_until(self, kind, timeout=5.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            event = self.read(timeout=max(0.1, deadline - time.time()))
            if event.get("evt") == kind:
                return event
        raise TimeoutError(f"没等到 {kind} 事件")

    def close(self):
        try:
            self.send(cmd="quit")
        except Exception:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def main():
    if not os.path.exists(BINARY):
        print(f"找不到二进制：{BINARY}（先 cargo build）")
        return 1

    canvas = Canvas(BINARY)
    failures = []
    try:
        canvas.send(cmd="hello", proto=PROTO)
        ready = canvas.read_until("ready")
        assert ready["proto"] == PROTO, ready
        print(f"✓ ready: 显示器 {ready['monitor']['width']}x{ready['monitor']['height']}")

        state = canvas.read_until("state")
        layers = state["layers"]
        assert len(layers) == 1, layers
        assert layers[0]["thumbnail_png"], "首帧应带缩略图"
        print(f"✓ 初始图层：{layers[0]['name']}，缩略图 {len(layers[0]['thumbnail_png'])}B")

        canvas.send(cmd="layer_add")
        state = canvas.read_until("state")
        assert len(state["layers"]) == 2, state["layers"]
        print("✓ 新建图层 → 2 层")

        canvas.send(cmd="layer_rename", index=1, name="冒烟层")
        state = canvas.read_until("state")
        assert state["layers"][1]["name"] == "冒烟层", state["layers"]
        print("✓ 改名生效")

        canvas.send(cmd="layer_visibility", index=0, visible=False)
        state = canvas.read_until("state")
        assert state["layers"][0]["visible"] is False, state["layers"]
        print("✓ 可见性切换生效")

        out = "/tmp/link-ipc-smoke.png"
        if os.path.exists(out):
            os.remove(out)
        canvas.send(
            cmd="export_image",
            path=out,
            format="png",
            background="blank",
        )
        exported = canvas.read_until("exported", timeout=15)
        assert exported["path"] == out, exported
        assert os.path.exists(out) and os.path.getsize(out) > 1000, "导出的 PNG 异常"
        print(f"✓ 导出空白背景 PNG: {os.path.getsize(out)} 字节")

        canvas.send(cmd="hello", proto=999)
        error = canvas.read_until("error")
        print(f"✓ 版本不匹配有错误事件：{error['message']}")
    except Exception as exc:  # noqa: BLE001
        failures.append(str(exc))
    finally:
        canvas.close()

    if failures:
        print("✗ 失败：" + "; ".join(failures))
        return 1
    print("全部通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
