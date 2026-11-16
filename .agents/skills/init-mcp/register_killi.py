#!/usr/bin/env python3
"""Set mcpServers.whatsapp in an mcp.json (killi / Claude Code style) to a binary.

Usage: register_killi.py <mcp.json> <whatsapp-mcp binary>

Backs up an existing file to <mcp.json>.bak-<timestamp> first. Other servers are
left as they are. Creates the file (mode 0600) if it doesn't exist.
"""

import datetime
import json
import os
import shutil
import sys


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    path = os.path.expanduser(sys.argv[1])
    binary = os.path.abspath(os.path.expanduser(sys.argv[2]))
    if not os.access(binary, os.X_OK):
        print(f"not executable: {binary}", file=sys.stderr)
        return 1

    data = {}
    if os.path.exists(path):
        with open(path) as f:
            data = json.load(f)
        backup = f"{path}.bak-{datetime.datetime.now():%Y%m%d-%H%M%S}"
        shutil.copy2(path, backup)
        print(f"backup: {backup}")

    servers = data.setdefault("mcpServers", {})
    replaced = "whatsapp" in servers
    servers["whatsapp"] = {"command": binary}

    tmp = f"{path}.tmp"
    with open(tmp, "w") as f:
        json.dump(data, f, indent=2, ensure_ascii=False)
        f.write("\n")
    os.chmod(tmp, 0o600)
    os.replace(tmp, path)
    print(f"whatsapp -> {binary} ({'replaced' if replaced else 'added'})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
