#!/usr/bin/env bash
# Build the desktop release binary, retrying only crates.io network failures.
#
# The macOS release job for 797dfde failed while resolving index.crates.io.
# Cargo's own retries were not enough. Compile errors must still fail the job
# on the first attempt.
set -euo pipefail

target="${1:?target triple required}"
max_attempts=5
attempt=1

network_failure() {
  grep -Eq \
    'Could not resolve host|Couldn'"'"'t resolve host|spurious network error|failed to get `.+` as a dependency|download of config.json failed|Temporary failure in name resolution|Network is unreachable|connection reset by peer|timed out|SSL connect error|curl: \(6\)|curl: \(28\)|error sending request for url' \
    "$1"
}

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
