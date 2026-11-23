"""Drive the actual censorprivacy xiaoo tui entry in a PTY; no substitute client."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import sys
import termios
import time
import urllib.request

ctl, config, prefix, xiaoo_root, origin, log = sys.argv[1:7]
tools_enabled = sys.argv[7:] == ["--enable-tools"]
task = "请复述：邮箱：tui@example.com；手机：13900139000。"


def get(path):
    with urllib.request.urlopen(origin + path, timeout=2) as response:
        return json.load(response)


previous = {session["id"] for session in get("/api/conversations")["sessions"]}
pid, terminal = pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"
    args = [ctl, "xiaoo", "tui", "--xiaoO-path", xiaoo_root,
            "--config-dir", config, "--prefix", prefix]
    if tools_enabled:
        args.append("--enable-tools")
    os.execv(ctl, args)

fcntl.ioctl(terminal, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
started = time.monotonic()
sent = False
output = bytearray()
recorded = None
exited = False
try:
    while time.monotonic() - started < 180:
        readable, _, _ = select.select([terminal], [], [], 0.15)
        if readable:
            try:
                chunk = os.read(terminal, 65536)
            except OSError:
                chunk = b""
            output.extend(chunk)
            if b"\x1b[6n" in chunk:
                os.write(terminal, b"\x1b[1;1R")
        if os.waitpid(pid, os.WNOHANG)[0]:
            exited = True
            raise RuntimeError("xiaoO TUI exited before completing its request")
        preparation_finished = not tools_enabled or "工具准备完成".encode() in output
        if not sent and preparation_finished and b"\x1b[?1049h" in output:
            time.sleep(0.5)
            input_started = time.monotonic()
            os.write(terminal, b"\x1b[200~" + task.encode() + b"\x1b[201~")
            time.sleep(0.2)
            os.write(terminal, b"\r")
            sent = True
        if sent:
            for session in get("/api/conversations")["sessions"]:
                if session["id"] in previous:
                    continue
                detail = get("/api/conversations/" + session["id"])
                for exchange in detail["exchanges"]:
                    if exchange["status"] == "error":
                        raise RuntimeError("TUI request failed: " + str(exchange["error"]))
                    if exchange["status"] == "completed" and "tui@example.com" in exchange["response"].get("content", ""):
                        assert any(task in snapshot["text"] for snapshot in exchange["snapshots"])
                        recorded = session["id"]
            if recorded:
                break
    assert recorded, "TUI conversation did not reach the gateway"
    print(json.dumps({"passed": True, "session_id": recorded,
                      "tools_enabled": tools_enabled,
                      "startup_to_input_seconds": round(input_started-started, 3),
                      "conversation_seconds": round(time.monotonic()-input_started, 3),
                      "entry": "censorprivacy xiaoo tui --xiaoO-path"}))
finally:
    Path(log).write_bytes(output)
    if not exited:
        os.write(terminal, b"\x03")
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if os.waitpid(pid, os.WNOHANG)[0]:
                exited = True
                break
            if select.select([terminal], [], [], 0.1)[0]:
                try:
                    os.read(terminal, 65536)
                except OSError:
                    pass
        if not exited:
            os.killpg(pid, signal.SIGTERM)
            os.waitpid(pid, 0)
    os.close(terminal)
