#!/usr/bin/env python3
"""Raw Minecraft login probe for the Aether auth layer.

Sends a minimal handshake + Login Start and reports whether the server
admitted the login or refused it, plus the refusal text. It does **not**
complete the Mojang encryption handshake, which is exactly what makes it
useful: an offline client (like MCC without credentials) cannot do that, so
the probe exercises the server's offline/hybrid decision path directly.

Used by auth.sh. Dependency-free (stdlib only).

CLI:
  auth-probe.py login <host> <port> <name> <protocol>
      -> prints "<status>\t<detail>"
  auth-probe.py burst <host> <port> <name> <protocol> <cap>
      -> logs in repeatedly until the server refuses or <cap> is reached;
         prints one "i status detail" line per attempt, then a final
         "admitted=<n> refusal=<detail>" line (empty detail if none).
"""
import socket
import struct
import sys

HANDSHAKE = 0x00
LOGIN_START = 0x00
# Login-state clientbound packet ids (the same ids in every version here).
CB_DISCONNECT = 0x00
CB_ENCRYPTION_REQUEST = 0x01
CB_LOGIN_SUCCESS = 0x02
CB_SET_COMPRESSION = 0x03
# next_state = login
STATE_LOGIN = 2


def varint(n: int) -> bytes:
    out = b""
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out += bytes([b | 0x80])
        else:
            out += bytes([b])
            return out


def read_varint(sock) -> int:
    num = 0
    for i in range(5):
        b = sock.recv(1)
        if not b:
            raise EOFError("connection closed")
        v = b[0]
        num |= (v & 0x7F) << (7 * i)
        if not v & 0x80:
            return num
    raise ValueError("varint too long")


def read_exact(sock, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise EOFError("connection closed")
        buf += chunk
    return buf


def mstring(s: str) -> bytes:
    b = s.encode("utf-8")
    return varint(len(b)) + b


def frame(payload: bytes) -> bytes:
    return varint(len(payload)) + payload


def probe(host: str, port: int, name: str, protocol: int, timeout: float = 8.0):
    """Return ('admitted'|'refused'|'encryption'|'unknown', detail)."""
    with socket.create_connection((host, port), timeout=timeout) as s:
        handshake = (
            varint(HANDSHAKE)
            + varint(protocol)
            + mstring(host)
            + struct.pack(">H", port)
            + varint(STATE_LOGIN)
        )
        s.sendall(frame(handshake))
        s.sendall(frame(varint(LOGIN_START) + mstring(name)))

        length = read_varint(s)
        body = read_exact(s, length)
        pid = body[0]  # these packet ids are single-byte
        if pid == CB_DISCONNECT:
            # The reason is a length-prefixed text component; surface just the
            # message text, which is all the harness matches on.
            rest = body[1:]
            n = i = 0
            while i < len(rest):
                b = rest[i]
                n |= (b & 0x7F) << (7 * i)
                i += 1
                if not b & 0x80:
                    break
            reason = rest[i : i + n]
            return "refused", reason.decode("utf-8", "replace")
        if pid in (CB_LOGIN_SUCCESS, CB_SET_COMPRESSION):
            return "admitted", f"id=0x{pid:02x}"
        if pid == CB_ENCRYPTION_REQUEST:
            return "encryption", "server requested Mojang auth"
        return "unknown", f"id=0x{pid:02x}"


def cmd_login(args):
    host, port, name, protocol = args[0], int(args[1]), args[2], int(args[3])
    status, detail = probe(host, port, name, protocol)
    print(f"{status}\t{detail}")


def cmd_burst(args):
    host, port, name, protocol, cap = (
        args[0],
        int(args[1]),
        args[2],
        int(args[3]),
        int(args[4]),
    )
    admitted = 0
    refusal = ""
    for i in range(cap):
        try:
            status, detail = probe(host, port, name, protocol)
        except OSError as e:
            status, detail = "error", str(e)
        print(f"{i + 1} {status} {detail[:100]}")
        if status == "admitted":
            admitted += 1
        else:
            refusal = f"{status}: {detail[:200]}"
            break
    print(f"admitted={admitted} refusal={refusal}")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(2)
    mode = sys.argv[1]
    if mode == "login":
        cmd_login(sys.argv[2:])
    elif mode == "burst":
        cmd_burst(sys.argv[2:])
    else:
        print(f"unknown mode {mode!r}", file=sys.stderr)
        sys.exit(2)
