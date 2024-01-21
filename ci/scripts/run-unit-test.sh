#!/usr/bin/env bash

set -euo pipefail

DIR=$(dirname "${BASH_SOURCE[0]}")

source "${DIR}/common/log.sh"

log::info "Run unit tests."

sccache --zero-stats > /dev/null

cov_file="lcov.info"
cargo llvm-cov nextest --all-features --lcov --output-path $cov_file

sccache --show-stats

log::info "Uploading Code Coverage."

$DIR/upload-codecov.sh $cov_file
