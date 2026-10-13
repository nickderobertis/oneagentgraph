#!/usr/bin/env bash
# Provision only the contract tier's native packaging seam. Other platforms
# inspect declarations; release.yml installs the same pin for its Darwin job.
set -euo pipefail
case "$(uname -s)" in Linux) ;; *) exit 0;; esac
for candidate in maturin "${CARGO_HOME:-$HOME/.cargo}/bin/maturin"; do
  if [ "$("$candidate" --version 2>/dev/null)" = 'maturin 1.14.1' ]; then exit 0; fi
done
cargo install --locked maturin --version 1.14.1
