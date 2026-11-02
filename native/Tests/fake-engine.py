#!/usr/bin/python3
"""Disposable JSONL engine used only by the native transport regression tests."""
import json
import os
from pathlib import Path
import sys
import threading
import time

lock = threading.Lock()
directory = Path(sys.argv[sys.argv.index("--data-dir") + 1])

startup_mode = directory / "startup-failure-mode"
if startup_mode.exists():
    mode = startup_mode.read_text()
    os.close(0)
    (directory / "stdin-closed").touch()
    if mode == "broken-input":
        time.sleep(0.04)  # Parent sees EPIPE before the startup frame arrives.
    sys.stderr.write("fake-secret-only-in-stderr\n")
    sys.stderr.flush()
    frame = (json.dumps({"id": 0, "status": 409, "body": {
        "code": "unknown_failure" if mode == "unknown" else "instance_in_use",
        "error": "fake-secret-never-display-this-text",
    }}) + "\n").encode()
    if mode == "split":
        for byte in frame:
            sys.stdout.buffer.write(bytes([byte]))
            sys.stdout.buffer.flush()
    else:
        sys.stdout.buffer.write(frame)
        sys.stdout.buffer.flush()
    sys.exit(1)

def reply(request, status=200, body=None, split=False):
    wire = (json.dumps({"id": request["id"], "status": status, "body": body}, ensure_ascii=False) + "\n").encode()
    with lock:
        if split:
            # Intentionally split inside multibyte UTF-8 sequences.
            for byte in wire:
                sys.stdout.buffer.write(bytes([byte]))
                sys.stdout.buffer.flush()
        else:
            sys.stdout.buffer.write(wire)
            sys.stdout.buffer.flush()

def delayed(request):
    time.sleep(0.2)
    reply(request, body={"late": True})

try:
    for line in sys.stdin.buffer:
        request = json.loads(line)
        path = request["path"]
        if path == "/api/echo":
            reply(request, body=request.get("body"), split=True)
        elif path == "/api/error":
            reply(request, status=422, body={"error": "A safe provider error."})
        elif path == "/api/delayed":
            threading.Thread(target=delayed, args=(request,), daemon=True).start()
        elif path == "/api/hang":
            pass
        elif path == "/api/stop-reading":
            reply(request, body={"ok": True})
            time.sleep(30)
        elif path == "/api/malformed":
            sys.stdout.buffer.write(b"not-json\n")
            sys.stdout.buffer.flush()
        elif path == "/api/oversized":
            sys.stdout.buffer.write(b"x" * (8 * 1024 * 1024 + 1))
            sys.stdout.buffer.flush()
        elif path == "/api/crash":
            os._exit(9)
        else:
            reply(request, body={"ok": True})
finally:
    (directory / "eof-observed").write_text("yes")
