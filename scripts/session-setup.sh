#!/usr/bin/env bash
# Idempotent, non-blocking session provisioner, wired into Claude Code's
# SessionStart hook (.claude/settings.json). Its job is to make a fresh web/cloud
# session able to run the documented `just` command surface with no manual steps.
#
# Only `just` is installed; the rest of the toolchain is reported rather than
# fetched, because a startup hook cannot reliably do a multi-minute install
# (`just bootstrap` does it when you are ready). CI provisions itself, so this
# no-ops there. Every step tolerates failure and the script always exits 0 — a
# flaky install must never abort session startup. Also safe to run by hand
# (`just session-setup`).
# llmlint: ignore-file[tool_output_is_signal, boundary_inputs_validated] a SessionStart hook must never abort a session, so failures log and continue instead of exiting non-zero; the only external input is a PyPI wheel fetched by uv, which validates it.
set -uo pipefail

# `just` floor. `rust-just` ships the prebuilt `just` binary on PyPI, so uv can
# fetch it with no Rust toolchain and no github.com reachability.
readonly JUST_MIN="1.51.0"
readonly BIN_DIR="$HOME/.local/bin"
# Capture the inherited PATH before we prepend BIN_DIR, so persist_session_env can
# tell whether BIN_DIR was already resolvable and only override PATH when it isn't.
readonly ORIG_PATH="${PATH}"

log() { printf 'session-setup: %s\n' "$*" >&2; }

# CI provisions the toolchain itself; skip there rather than racing it.
{ [ -n "${CI:-}" ] || [ -n "${GITHUB_ACTIONS:-}" ]; } && exit 0

export PATH="${BIN_DIR}:${PATH}"

ensure_just() {
  command -v just >/dev/null 2>&1 && return 0
  if ! command -v uv >/dev/null 2>&1; then
    log "uv not found; cannot install just (install uv: https://docs.astral.sh/uv/)"
    return 0
  fi
  log "installing rust-just >= $JUST_MIN via uv tool"
  uv tool install --upgrade "rust-just>=$JUST_MIN" >&2 \
    || log "rust-just install failed (continuing)"
}

# Verify the rest of the toolchain and point at what's missing.
verify_prereqs() {
  local tool
  for tool in uv rustup cargo; do
    command -v "$tool" >/dev/null 2>&1 \
      || log "$tool not on PATH (install rustup: https://rustup.rs/ ; uv: https://docs.astral.sh/uv/)"
  done
  if command -v cargo >/dev/null 2>&1 && ! command -v cargo-nextest >/dev/null 2>&1; then
    log "cargo-nextest/cargo-llvm-cov missing — run 'just bootstrap' before 'just check'"
  fi
}

# Persist PATH so the freshly installed `just` resolves in every later Bash call.
persist_session_env() {
  [ -n "${CLAUDE_ENV_FILE:-}" ] || return 0
  case ":${ORIG_PATH}:" in
    *":${BIN_DIR}:"*) ;;
    *) printf 'export PATH=%q\n' "${BIN_DIR}:${PATH}" >> "$CLAUDE_ENV_FILE" ;;
  esac
}

ensure_just
verify_prereqs
persist_session_env

# Hand off to the llmlint-tier installer beside this script.
setup_llmlint="$(dirname "$0")/setup-llmlint.sh"
if [ -x "$setup_llmlint" ]; then
  "$setup_llmlint" || log "setup-llmlint.sh reported an issue (continuing)"
fi

exit 0
