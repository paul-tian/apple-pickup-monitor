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
