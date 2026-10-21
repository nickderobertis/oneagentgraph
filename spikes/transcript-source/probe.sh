#!/usr/bin/env bash
# Spike harness: does oneharness's history file serve as the full transcript a
# journal turn record could point at? See README.md beside this file.
#
#   spikes/transcript-source/probe.sh            # two real turns, then the recorded runs
#   spikes/transcript-source/probe.sh recorded   # recorded runs only, no quota spent
#   spikes/transcript-source/probe.sh replay --scratch DIR   # re-resolve a saved probe, no quota
#
# Needs `oneagentgraph`, `oneharness` and `python3` on PATH. The two live turns
# cost real quota: one short prompt each.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 -I "$here/probe.py" "$@"
