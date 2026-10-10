#!/usr/bin/env bash
# Build the desktop release binary, retrying only crates.io network failures.
#
# The macOS release job for 797dfde (run 38037377671) failed in
# "Build release binary". Cargo exhausted its own resolver retries in about
# 80 seconds:
#   Could not resolve host: index.crates.io
#   download of config.json failed
# The step was a single `cargo build` with no outer retry, so release-publish
# was skipped. Compile errors must still fail on the first attempt.
set -euo pipefail

strip_ansi() {
  # Cargo colors the log when CARGO_TERM_COLOR=always. Match the text.
  sed $'s/\x1b\\[[0-9;]*[[:alpha:]]//g' "$1"
}

network_failure() {
  local stripped status
  stripped="$(mktemp)"
  strip_ansi "$1" >"${stripped}"
  if grep -Eq \
    'Could not resolve host|Couldn'"'"'t resolve host|spurious network error|download of config.json failed|Temporary failure in name resolution|Network is unreachable|connection reset by peer|SSL connect error|curl: \(6\)|curl: \(28\)|error sending request for url|failed to download|checksum failed for ' \
    "${stripped}"; then
    status=0
  else
    status=1
  fi
  rm -f "${stripped}"
  return "${status}"
}

warm_index() {
  # The 797dfde failure died on the first sparse-index config download.
  # Probe that URL before cargo starts, with a pause between DNS blips.
  local attempt
  for attempt in 1 2 3 4 5; do
    if curl --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 45 \
      -fsS "https://index.crates.io/config.json" -o /dev/null; then
      return 0
    fi
    echo "index.crates.io lookup failed (attempt ${attempt}/5); retrying" >&2
    sleep $((attempt * 5))
  done
  echo "index.crates.io still unreachable; cargo build will retry the download" >&2
}

self_test() {
  local dns compile checksum ansi
  dns="$(mktemp)"
  compile="$(mktemp)"
  checksum="$(mktemp)"
  ansi="$(mktemp)"
  cat >"${dns}" <<'EOF'
error: failed to get `async-trait` as a dependency of package `mycode-core v0.9.27`
Caused by:
  failed to load source for dependency `async-trait`
  download of config.json failed
  [6] Couldn't resolve host name (Could not resolve host: index.crates.io)
EOF
  cat >"${compile}" <<'EOF'
error[E0308]: mismatched types
   --> crates/mycode-tools/src/builtin/shell.rs:1:1
EOF
  echo 'error: checksum failed for `libc v0.2.190`' >"${checksum}"
  printf '\033[1m\033[91merror\033[0m: download of config.json failed\nCould not resolve host: index.crates.io\n' >"${ansi}"
  network_failure "${dns}"
  network_failure "${checksum}"
  network_failure "${ansi}"
  if network_failure "${compile}"; then
    echo "compile error was treated as a network failure" >&2
    exit 1
  fi
  rm -f "${dns}" "${compile}" "${checksum}" "${ansi}"
  echo "cargo-release-build self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
  exit 0
fi

target="${1:?target triple required}"
export CARGO_NET_RETRY="${CARGO_NET_RETRY:-10}"
export CARGO_HTTP_TIMEOUT="${CARGO_HTTP_TIMEOUT:-60}"
max_attempts=5
attempt=1

warm_index

while true; do
  log="$(mktemp)"
  set +e
  cargo build --release --locked -p mycode-desktop --target "${target}" >"${log}" 2>&1
  status=$?
  set -e
  cat "${log}"
  if [[ "${status}" -eq 0 ]]; then
    rm -f "${log}"
    exit 0
  fi
  if [[ "${attempt}" -ge "${max_attempts}" ]] || ! network_failure "${log}"; then
    rm -f "${log}"
    exit "${status}"
  fi
  echo "cargo build hit a crates.io network error (attempt ${attempt}/${max_attempts}); retrying" >&2
  rm -f "${log}"
  sleep $((attempt * 15))
  attempt=$((attempt + 1))
done
