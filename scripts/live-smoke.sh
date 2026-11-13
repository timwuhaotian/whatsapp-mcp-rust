#!/usr/bin/env bash
# Thin wrapper: same live checks, needs python3 + a built binary.
# Usage: bash scripts/live-smoke.sh [--data-dir DIR] [--bin PATH]
set -euo pipefail
cd "$(dirname "$0")/.."
exec python3 scripts/live-smoke.py "$@"
