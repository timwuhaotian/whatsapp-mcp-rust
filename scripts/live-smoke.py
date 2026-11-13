#!/usr/bin/env python3
"""Live verification for a paired whatsapp-mcp session.

Precondition: `whatsapp-mcp login` completed (QR scanned on the phone).
Usage:  python3 scripts/live-smoke.py [--data-dir DIR] [--bin PATH]

Runs 5 checks over a stdio MCP session and prints PASS/FAIL/SKIP per item.
Exit 0 only if nothing FAILs.
"""
import argparse
import base64
import json
import os
import struct
import subprocess
import sys
import tempfile
import zlib

PASS, FAIL, SKIP = "PASS", "FAIL", "SKIP"
results: list[tuple[str, str, str]] = []


def check(name: str, status: str, detail: str = "") -> None:
    results.append((name, status, detail))
    print(f"[{status}] {name}" + (f" — {detail}" if detail else ""), flush=True)


def rpc(proc: subprocess.Popen, msg_id: int, method: str, params=None):
    msg = {"jsonrpc": "2.0", "id": msg_id, "method": method}
    if params is not None:
        msg["params"] = params
    proc.stdin.write(json.dumps(msg) + "\n")
    proc.stdin.flush()
    return json.loads(proc.stdout.readline())


def call(proc, msg_id: int, tool: str, args: dict):
    res = rpc(proc, msg_id, "tools/call", {"name": tool, "arguments": args})
    if "error" in res:
        return None, res["error"]
    content = res["result"]["content"]
    texts = [c["text"] for c in content if c.get("type") == "text"]
    structured = res["result"].get("structuredContent")
    return (structured if structured is not None else texts), None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--data-dir", default=None)
    ap.add_argument("--bin", default="./target/release/whatsapp-mcp")
    args = ap.parse_args()

    cmd = [args.bin]
    if args.data_dir:
        cmd += ["--data-dir", args.data_dir]
    try:
        proc = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True, bufsize=1,
        )
    except FileNotFoundError:
        check("spawn", FAIL, f"binary not found: {args.bin}")
        return 1

    msg_id = 1
    rpc(proc, msg_id, "initialize", {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "live-smoke", "version": "0"},
    })
    msg_id += 1
    proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    proc.stdin.flush()

    # 1. status: connected + own_jid
    msg_id += 1
    status, err = call(proc, msg_id, "whatsapp_status", {})
    own_jid = (status or {}).get("own_jid", "") if isinstance(status, dict) else ""
    if err or not status:
        check("whatsapp_status connected", FAIL, f"tool error: {err}")
        proc.kill()
        return report()
    if status.get("connected") and own_jid:
        check("whatsapp_status connected", PASS, f"own_jid={own_jid}")
    else:
        check("whatsapp_status connected", FAIL,
              f"connected={status.get('connected')} own_jid={own_jid!r} — is the phone paired? run login first")
        proc.kill()
        return report()
    own_number = own_jid.split("@")[0]

    # 2. history landed
    msg_id += 1
    chats, _ = call(proc, msg_id, "list_chats", {"limit": 5})
    msg_id += 1
    contacts, _ = call(proc, msg_id, "search_contacts", {"query": "a"})
    if isinstance(chats, list) and len(chats) > 0:
        check("list_chats non-empty (history sync landed)", PASS, f"{len(chats)} chats")
    else:
        check("list_chats non-empty (history sync landed)", FAIL, "no chats — history sync missing?")
    if isinstance(contacts, list) and len(contacts) > 0:
        check("search_contacts returns results", PASS, f"{len(contacts)} contacts")
    else:
        check("search_contacts returns results", FAIL, "no contacts")

    # 3. send text to self, read it back
    probe = "live-smoke probe 1"
    msg_id += 1
    sent, _ = call(proc, msg_id, "send_message", {"recipient": own_number, "message": probe})
    ok = isinstance(sent, dict) and sent.get("success") is True
    if not ok:
        check("send_message to self", FAIL, json.dumps(sent)[:200])
    else:
        msg_id += 1
        found, _ = call(proc, msg_id, "list_messages",
                         {"chat_jid": own_jid, "query": probe, "include_context": False, "limit": 5})
        hit = isinstance(found, list) and any(m.get("content") == probe for m in found)
        check("send_message to self", PASS if hit else FAIL,
              "round-trip readable" if hit else "sent but not readable back")

    # 4. send small png to self, download it back
    png = make_png()
    with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as f:
        f.write(png)
        png_path = f.name
    try:
        msg_id += 1
        sent, _ = call(proc, msg_id, "send_file",
                        {"recipient": own_number, "media_path": png_path, "caption": "smoke"})
        if not (isinstance(sent, dict) and sent.get("success") is True):
            check("send_file to self", FAIL, json.dumps(sent)[:200])
        else:
            msg_id += 1
            found, _ = call(proc, msg_id, "list_messages",
                             {"chat_jid": own_jid, "query": "smoke", "include_context": False, "limit": 10})
            media = [m for m in (found or []) if isinstance(m, dict) and m.get("media_type")]
            if not media:
                check("send_file to self", FAIL, "sent but no media message found")
            else:
                m = media[0]
                msg_id += 1
                dl, _ = call(proc, msg_id, "download_media",
                              {"message_id": m["id"], "chat_jid": m["chat_jid"]})
                path = (dl or {}).get("file_path", "") if isinstance(dl, dict) else ""
                if path and os.path.isfile(path) and os.path.getsize(path) > 0:
                    check("send_file + download_media round-trip", PASS,
                          f"{os.path.getsize(path)} bytes at {path}")
                else:
                    check("send_file + download_media round-trip", FAIL, f"bad path: {path!r}")
    finally:
        os.unlink(png_path)

    # 5. audio (ffmpeg-gated)
    msg_id += 1
    wav = make_wav()
    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
        f.write(wav)
        wav_path = f.name
    try:
        import shutil
        if shutil.which("ffmpeg") is None:
            check("send_audio_message", SKIP, "ffmpeg not installed")
        else:
            msg_id += 1
            sent, _ = call(proc, msg_id, "send_audio_message",
                            {"recipient": own_number, "media_path": wav_path})
            ok = isinstance(sent, dict) and sent.get("success") is True
            check("send_audio_message to self", PASS if ok else FAIL, json.dumps(sent)[:200])
    finally:
        os.unlink(wav_path)

    proc.stdin.close()
    try:
        proc.wait(timeout=5)
    except Exception:
        proc.kill()
    return report()


def report() -> int:
    failed = [r for r in results if r[1] == FAIL]
    print(f"--- {len(results) - len(failed)}/{len(results)} non-failed ---")
    return 1 if failed else 0


def make_png() -> bytes:
    """Minimal valid 8x8 red PNG (zlib-compressed raw scanlines)."""
    def chunk(typ: bytes, data: bytes) -> bytes:
        c = typ + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + b"\xff\x00\x00" * 8 for _ in range(8))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", 8, 8, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw))
            + chunk(b"IEND", b""))


def make_wav() -> bytes:
    """1s 440Hz mono 16-bit 24kHz WAV (ffmpeg converts to opus ogg)."""
    import math
    sr, secs, freq = 24000, 1, 440
    samples = b"".join(
        struct.pack("<h", int(10000 * math.sin(2 * math.pi * freq * i / sr)))
        for i in range(sr * secs)
    )
    return (b"RIFF" + struct.pack("<I", 36 + len(samples)) + b"WAVEfmt "
            + struct.pack("<IHHIIHH", 16, 1, 1, sr, sr * 2, 2, 16)
            + b"data" + struct.pack("<I", len(samples)) + samples)


if __name__ == "__main__":
    sys.exit(main())
