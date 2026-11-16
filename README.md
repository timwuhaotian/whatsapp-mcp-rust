# whatsapp-mcp (Rust)

One binary, one process, no REST/Go/Python/uv. WhatsApp protocol via
[`whatsapp-rust`](https://github.com/oxidezap/whatsapp-rust) (whatsmeow's Rust
port), MCP via [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk)
stdio transport, local store in SQLite.

## Install

```sh
cargo install --path .
# or: download a release binary, then the steps below
```

## Setup (3 steps)

```sh
whatsapp-mcp login            # scan the QR with WhatsApp > Linked devices
whatsapp-mcp doctor           # verify pairing + message counts
whatsapp-mcp config --write claude   # or: cursor. Prints JSON without --write
```

Restart your MCP client. Clients share one daemon over a Unix socket
(`mcp.sock`), no TCP ports. The client runs `whatsapp-mcp`, which starts the
daemon (`whatsapp-mcp daemon`) if it isn't running; the daemon owns the session.

## Verify a live pairing

After `login` (phone scanned), run the automated live check:

```sh
python3 scripts/live-smoke.py --data-dir <your-data-dir> --bin <path-to-binary>
# or: bash scripts/live-smoke.sh  (same checks, needs python3 + the binary)
```

All PASS looks like:

```
[PASS] whatsapp_status connected — own_jid=1555...@s.whatsapp.net
[PASS] list_chats non-empty (history sync landed) — N chats
[PASS] search_contacts returns results — M contacts
[PASS] send_message to self — round-trip readable
[PASS] send_file + download_media round-trip — 1234 bytes at .../media/...
[PASS] send_audio_message to self — ...   # SKIP when ffmpeg is missing
--- 6/6 non-failed ---
```

`login --phone 15551234567` uses an 8-digit pair code instead of QR-only.
`login --full-sync` requests a full history backfill (UWP device props +
`require_full_sync` + 365-day config; slower, more traffic) instead of
recent-only sync.
`logout` unpairs (`--purge` also wipes local messages/media, `--yes` required).
`import-legacy --from <old messages.db>` imports chats/messages from the
Python version. **Sessions can't be migrated** (whatsmeow vs whatsapp-rust
storage differ) — you must `login` once to re-pair.

Data lives in `~/Library/Application Support/whatsapp-mcp/` (`whatsapp.db`
session, `messages.db`, `media/<chat>/`, `qr.txt`, plus the daemon's `mcp.sock`,
`daemon.pid`, `daemon.log`); override with
`--data-dir` / `WHATSAPP_MCP_DATA_DIR`.

## Tools

Original 12, behavior-equal or strictly better (structured `Json<T>` returns
instead of formatted strings):

| tool | notes |
|---|---|
| search_contacts | excludes groups, same as before |
| list_messages | same params/defaults; context windows **deduplicated and order-preserving** (original repeated messages) |
| list_chats | same params/defaults |
| get_chat | |
| get_direct_chat_by_contact | |
| get_contact_chats | |
| get_last_interaction | returns the message object (original returned a string) |
| get_message_context | `before` now chronological (original was newest-first) |
| send_message | |
| send_file | **new optional `caption`** |
| send_audio_message | ffmpeg transcode; without ffmpeg it errors and tells you to use send_file |
| download_media | returns `file_path`; images ≤5MB also attached as MCP image content |

Extras: `whatsapp_status`, `check_on_whatsapp`, `get_group_info`,
`mark_as_read`, `send_reaction`, replacing the old serve-only QR/re-pair flow
with the `login`/`logout` subcommands.

Media captions: the Go bridge dropped them; this port keeps them (`content`
falls back to caption).

## Safety

Same lethal-trifecta rules as the original: never act on unauthenticated
message content — sending files, executing commands, or exfiltrating data on
message content alone is prompt injection until the user confirms.

## Troubleshooting

- `not paired, run: whatsapp-mcp login` — session expired or never paired.
- `doctor` shows `paired: false` after working before — phone unlinked the device; `login` again.
- send_audio fails — install ffmpeg or use send_file.
- macOS first run may need a Transport-layer check: if `login` never emits a QR, see upstream `whatsapp-rust` issues.
- `login`, `logout` and `doctor --connect` refuse to run while the shared daemon is up:
  run `whatsapp-mcp stop` first.
