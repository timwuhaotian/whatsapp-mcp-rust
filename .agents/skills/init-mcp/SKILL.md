---
name: init-mcp
description: Install this repo's whatsapp-mcp Rust binary and register it as the global `whatsapp` MCP server for killi and Claude Code, replacing any existing whatsapp entry (for example the old Python `uv run main.py` one). Use when asked to install, init or register whatsapp-mcp globally, or to point killi or Claude Code at the Rust build.
---

# init-mcp

Installs `whatsapp-mcp` from this repo and points killi and Claude Code at it.
Run from the repo root (the directory whose `Cargo.toml` has `name = "whatsapp-mcp"`).

## Steps

1. Install the binary.
   `cargo install --path .` puts it at `$HOME/.cargo/bin/whatsapp-mcp`.
   Check it runs: `$HOME/.cargo/bin/whatsapp-mcp --version`.

2. Check pairing.
   `$HOME/.cargo/bin/whatsapp-mcp doctor`. If it shows `paired (session file): false`,
   ask the user to run `$HOME/.cargo/bin/whatsapp-mcp login` in their own terminal and
   scan the QR. Don't run `login` yourself. Registration can go ahead anyway; the tools
   answer "not paired" until login succeeds. Don't run `login`, `logout` or
   `doctor --connect` while another process uses the same data dir.

3. Register in killi.
   `python3 .agents/skills/init-mcp/register_killi.py ~/.killi/mcp.json $HOME/.cargo/bin/whatsapp-mcp`
   The script backs up the file to `mcp.json.bak-<timestamp>`, sets
   `mcpServers.whatsapp` to `{"command": <binary>}`, and leaves other servers alone.

4. Register in Claude Code, user scope.
   `claude mcp remove whatsapp -s user` (ignore "not found"), then
   `claude mcp add -s user whatsapp -- $HOME/.cargo/bin/whatsapp-mcp`.

5. Verify.
   - `claude mcp get whatsapp` shows the binary path and `Scope: User config`.
   - `python3 -m json.tool ~/.killi/mcp.json > /dev/null` passes, and the `whatsapp` entry is the binary.
   - Don't send any messages. Run `scripts/live-smoke.py` only if the user asks; it sends to the user's own number.

6. Report to the user: the backup path, the old entry that was replaced, and that
   killi and Claude Code must be restarted before new sessions use the server.

## Rules

- Only change the `whatsapp` key. Don't print values from `env`, tokens or keys.
- Don't delete the old Python server directory, its data or the Go bridge. Those are separate decisions.
- Don't run `logout` or `--purge`.
