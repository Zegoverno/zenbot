#!/usr/bin/env python3
"""Send one message on a session's WebSocket, as zen's terminal app does, and print the events
that follow as JSON lines: until the turn's `end`, then for up to WAIT seconds more (the kernel's
after-turn events: `title`, `suggestion`). Standard library only.

    ws_prompt.py <base url> <token> <session id> '<json message>' [wait seconds]
"""
import base64
import json
import os
import socket
import struct
import sys
import time
from urllib.parse import urlparse


def frame(text):
    data = text.encode()
    head = bytes([0x81])
    n = len(data)
    if n < 126:
        head += bytes([0x80 | n])
    elif n < 65536:
        head += bytes([0x80 | 126]) + struct.pack(">H", n)
    else:
        head += bytes([0x80 | 127]) + struct.pack(">Q", n)
    mask = os.urandom(4)
    return head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data))


def read_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise EOFError
        buf += chunk
    return buf


def read_frame(sock):
    b1, b2 = read_exact(sock, 2)
    n = b2 & 0x7F
    if n == 126:
        n = struct.unpack(">H", read_exact(sock, 2))[0]
    elif n == 127:
        n = struct.unpack(">Q", read_exact(sock, 8))[0]
    return b1 & 0x0F, read_exact(sock, n)


def main():
    base, token, session, message = sys.argv[1:5]
    wait = float(sys.argv[5]) if len(sys.argv) > 5 else 10
    u = urlparse(base)
    sock = socket.create_connection((u.hostname, u.port or 80), timeout=120)
    key = base64.b64encode(os.urandom(16)).decode()
    sock.sendall(
        (f"GET /api/sessions/{session}/ws HTTP/1.1\r\nHost: {u.netloc}\r\nUpgrade: websocket\r\n"
         f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
         f"Authorization: Bearer {token}\r\n\r\n").encode())
    head = b""
    while b"\r\n\r\n" not in head:
        head += sock.recv(1)
    if b" 101 " not in head.split(b"\r\n")[0]:
        sys.exit("no upgrade: " + head.decode(errors="replace").splitlines()[0])
    sock.sendall(frame(message))
    deadline = None
    while True:
        if deadline is not None:
            left = deadline - time.time()
            if left <= 0:
                break
            sock.settimeout(left)
        try:
            op, data = read_frame(sock)
        except (socket.timeout, EOFError):
            break
        if op == 8:
            break
        if op != 1:
            continue
        ev = json.loads(data)
        print(json.dumps(ev), flush=True)
        if ev.get("type") == "end" and deadline is None:
            deadline = time.time() + wait
        if ev.get("type") == "suggestion":
            break


if __name__ == "__main__":
    main()
