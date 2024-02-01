#!/usr/bin/env bash

source "$(dirname "${BASH_SOURCE[0]}")/common/log.sh"

if [ -n "$CODECOV_TOKEN" ]; then
  token="-t ${CODECOV_TOKEN}"
else
  token=""
fi

set -euo pipefail
curl -Os https://uploader.codecov.io/latest/linux/codecov && chmod +x codecov

max_retry=${MAX_RETRY-20}
file=${1-"lcov.info"}
retry_delay=${RETRY_DELAY-30}

counter=0

set +e

# Sometimes uploading to codecov might reach the Github API rate limit
while true; do
    ./codecov -f $file -Z $token
    if [ $? -eq 0 ]; then
        log::info "Uploaded!"
        break
    fi

    ((counter++))

    if [ $counter -eq $max_retry ]; then
        log::fatal "Max retries reached. Exiting."
    fi
    log::warn "Fail to upload, retring ${counter}/${max_retry} in ${retry_delay}s."
    sleep $retry_delay
done
