#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
dsh-desktop-shell-tauri 冒烟测试。

把任务书 13.2 验收清单中可无头自动化的部分固化为脚本，覆盖壳的核心逻辑：

- 控制通道 token 门禁（恶意网页模拟：无 token 的请求必须 403，应用不退出）
- 端口退让（3177 被占时自动选 3178）
- 端口全占的失败路径（进程不崩、控制通道仍活着）
- 单实例握手（第二实例快速退出，聚焦交给第一实例）
- 锁端口被无关程序占用时拒绝启动（exit 1）
- 退出无孤儿（kill 主进程后 3175/3176/3177-3186 全部可重新绑定）
- 退出后立即重启正常

前置条件（见 README「自动化冒烟测试」）：
- 已 npm install（工作区 node_modules/@deepseek-ai/dsh 存在）
- 已 cargo build（dev exe 存在）
- 没有正在运行的应用实例（脚本会前置检查 3175/3176）

运行：python scripts/smoke-test.py
测试期间应用窗口会短暂弹出数次；dev/release 共用 DSH_HOME（已知坑 #13），
本脚本只调用只读端点，不写背景/凭证数据。

手工项（脚本不覆盖，仍走 13.2 清单）：splash 视觉跳转、背景选择对话框、
右键菜单行为、关于对话框、>10MB 背景流畅度、覆盖安装/卸载。
"""

import os
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

try:
    sys.stdout.reconfigure(encoding="utf-8")
except AttributeError:
    pass

HOST = "127.0.0.1"
PORT_FIRST, PORT_LAST = 3177, 3186
LOCK_PORT, CONTROL_PORT = 3176, 3175
SHELL_EXE = os.environ.get(
    "DSH_SHELL_EXE",
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src-tauri", "target", "debug", "dsh-desktop-shell-tauri.exe"),
)
DSH_BIN = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js")

fails = []


def check(cond, name, detail=""):
    mark = "PASS" if cond else "FAIL"
    print(f"[{mark}] {name}" + (f" — {detail}" if detail else ""))
    if not cond:
        fails.append(name)
    return cond


def port_bindable(port):
    s = socket.socket()
    try:
        s.bind((HOST, port))
        return True
    except OSError:
        return False
    finally:
        s.close()


def occupy(ports):
    """Bind+listen 占住端口列表，返回 socket 列表（调用方负责 close）。"""
    socks = []
    for p in ports:
        s = socket.socket()
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind((HOST, p))
        s.listen(1)
        socks.append(s)
    return socks


def http_code(port, path, timeout=3):
    """GET 指定端口路径，返回状态码；连不上返回 None。"""
    req = urllib.request.Request(f"http://{HOST}:{port}{path}", method="GET")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code
    except OSError:
        return None


def wait_ready(timeout=60):
    """轮询 3177-3186 直到某端口对 GET / 返回 2xx/3xx，返回该端口；超时返回 None。"""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for port in range(PORT_FIRST, PORT_LAST + 1):
            code = http_code(port, "/", timeout=1)
            if code is not None and 200 <= code < 400:
                return port
        time.sleep(0.5)
    return None


def start_shell():
    return subprocess.Popen(
        [SHELL_EXE],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def stop_shell(proc, grace=2.0):
    """kill 主进程，并留出 Job Object 杀子进程树的余量。"""
    if proc.poll() is None:
        proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=10)
    time.sleep(grace)


def test_control_channel_gated():
    print("\n== 1. 控制通道 token 门禁（无 token 全 403，应用不退出）==")
    proc = start_shell()
    try:
        ready = wait_ready()
        check(ready is not None, "dsh 后端就绪", f"port={ready}")
        for path in ["/win/quit", "/win/state", "/bg/state", "/bg/opacity?v=0.3", "/win/min"]:
            code = http_code(CONTROL_PORT, path)
            check(code == 403, f"GET {path} 无 token → 403", f"got {code}")
        time.sleep(2)
        check(proc.poll() is None, "恶意 /win/quit 后应用仍存活")
    finally:
        stop_shell(proc)


def test_port_fallback():
    print("\n== 2. 端口退让（占 3177，自动选 3178）==")
    held = occupy([PORT_FIRST])
    try:
        proc = start_shell()
        try:
            ready = wait_ready()
            check(ready is not None and ready != PORT_FIRST, "退让后就绪端口 ≠ 3177", f"port={ready}")
        finally:
            stop_shell(proc)
    finally:
        for s in held:
            s.close()


def test_port_exhausted():
    print("\n== 3. 端口全占（3177-3186 全占，进程不崩、控制通道仍活）==")
    held = occupy(list(range(PORT_FIRST, PORT_LAST + 1)))
    try:
        proc = start_shell()
        try:
            time.sleep(8)  # 等 boot 线程跑完 pick_port panic → catch_unwind 失败路径
            check(proc.poll() is None, "端口全占时进程不退出")
            code = http_code(CONTROL_PORT, "/bg/state")
            check(code == 403, "控制通道仍存活且带门禁", f"got {code}")
        finally:
            stop_shell(proc)
    finally:
        for s in held:
            s.close()


def test_single_instance():
    print("\n== 4. 单实例握手（第二实例快速退出，第一实例存活）==")
    first = start_shell()
    try:
        check(wait_ready() is not None, "第一实例就绪")
        t0 = time.monotonic()
        second = start_shell()
        try:
            rc = second.wait(timeout=15)
            elapsed = time.monotonic() - t0
            check(rc == 0, "第二实例退出码 0", f"rc={rc}, {elapsed:.1f}s")
            check(elapsed < 10, "第二实例快速退出（无后端拉起）", f"{elapsed:.1f}s")
        finally:
            if second.poll() is None:
                stop_shell(second)
        time.sleep(1)
        check(first.poll() is None, "第一实例仍存活")
    finally:
        stop_shell(first)


def test_lock_squatter():
    print("\n== 5. 锁端口被无关程序占用（拒绝启动，exit 1）==")
    held = occupy([LOCK_PORT])
    try:
        proc = start_shell()
        try:
            rc = proc.wait(timeout=20)
            err = proc.stderr.read().decode(errors="replace") if proc.stderr else ""
            check(rc == 1, "退出码 1", f"rc={rc}")
            check("refusing to start" in err, "stderr 给出明确原因", f"stderr={err.strip()[:80]!r}")
        finally:
            if proc.poll() is None:
                stop_shell(proc)
    finally:
        for s in held:
            s.close()


def test_exit_no_orphan_and_restart():
    print("\n== 6. 退出无孤儿 + 立即重启 ==")
    proc = start_shell()
    try:
        ready = wait_ready()
        check(ready is not None, "实例就绪", f"port={ready}")
        stop_shell(proc)
        bindable = [p for p in [CONTROL_PORT, LOCK_PORT] + list(range(PORT_FIRST, PORT_LAST + 1)) if port_bindable(p)]
        check(len(bindable) == 12, "kill 后全部端口可重新绑定（无孤儿进程持口）", f"{len(bindable)}/12")
    finally:
        if proc.poll() is None:
            stop_shell(proc)
    # 立即重启
    proc = start_shell()
    try:
        t0 = time.monotonic()
        ready = wait_ready()
        check(ready is not None, "kill 后立即重启就绪", f"port={ready}, {time.monotonic() - t0:.1f}s")
    finally:
        stop_shell(proc)


def main():
    print(f"smoke-test: exe={SHELL_EXE}")
    if not os.path.isfile(SHELL_EXE):
        print(f"[FAIL] 找不到 dev exe：{SHELL_EXE}\n      先执行 `cd src-tauri && cargo build`（或 npm run dev 一次）")
        return 1
    if not os.path.isfile(DSH_BIN):
        print(f"[FAIL] 找不到 dsh 运行时：{DSH_BIN}\n      先在工作区执行 `npm install`")
        return 1
    for port in (CONTROL_PORT, LOCK_PORT):
        if not port_bindable(port):
            print(f"[FAIL] 端口 {port} 被占用——请先退出正在运行的应用实例再跑测试")
            return 1

    tests = [
        test_control_channel_gated,
        test_port_fallback,
        test_port_exhausted,
        test_single_instance,
        test_lock_squatter,
        test_exit_no_orphan_and_restart,
    ]
    for t in tests:
        try:
            t()
        except Exception as e:
            print(f"[FAIL] {t.__name__} 异常：{e!r}")
            fails.append(t.__name__)

    print("\n" + "=" * 50)
    if fails:
        print(f"结果：{len(fails)} 项失败 — {', '.join(fails)}")
        return 1
    print("结果：全部通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
