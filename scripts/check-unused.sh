#!/bin/sh
set -eu

cd "$(dirname "$0")/.."
exec cargo check --workspace --all-targets --all-features --locked
