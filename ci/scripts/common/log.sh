${__COMMON_LOG__:=false} && return 0 || __COMMON_LOG__=true

# ENVIRONMENT VARIABLES
#   DEBUG: If set to true, debug messages will be printed to stdout
function log::debug() {
  if [[ "${DEBUG:=false}" == "true" ]]; then
    echo -e "\033[00;34m" "[DEBUG]" "$@" "\033[0m"
  fi
}

function log::info() {
  echo -e "\033[00;32m" "[INFO]" "$@" "\033[0m"
}

function log::warn() {
  echo -e "\033[00;33m" "[WARN]" "$@" "\033[0m"
}

function log::error() {
  echo -e "\033[00;31m" "[ERROR]" "$@" "\033[0m"
}

function log::fatal() {
  echo -e "\033[00;31m" "[FATAL]" "$@" "\033[0m"
  exit 1
}
