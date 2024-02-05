#!/usr/bin/env bash

set -euo pipefail

DIR=$(dirname "${BASH_SOURCE[0]}")

source "${DIR}/common/log.sh"

log::info "Run basic checking."

cargo audit
cargo fmt --all -- --check
cargo sort --check
cargo machete

$DIR/run-unit-test.sh
