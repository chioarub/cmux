#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_DIR"

DOCTOR_ONLY=0
RELEASE=0
RESET=0

usage() {
    cat <<'EOF'
Usage: ./scripts/run-linux.sh [OPTIONS] [-- <binary-args>]

Build and launch the cmux Linux app.

Options:
  --release    Build and run with optimizations
  --doctor     Print the Linux/Wayland readiness report and exit
  --reset      Clear saved session state before launching
  -h, --help   Show this help

Socket:
  Defaults to $XDG_RUNTIME_DIR/cmux.sock
  Override with CMUX_SOCKET or CMUX_SOCKET_PATH
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release) RELEASE=1; shift ;;
        --doctor)  DOCTOR_ONLY=1; shift ;;
        --reset)   RESET=1; shift ;;
        --)        shift; break ;;
        -h|--help) usage; exit 0 ;;
        *)         break ;;
    esac
done

if [[ -f "$SCRIPT_DIR/linux_wayland_doctor.py" ]]; then
    python3 "$SCRIPT_DIR/linux_wayland_doctor.py" >/tmp/cmux-linux-doctor.txt 2>&1 || {
        cat /tmp/cmux-linux-doctor.txt
        exit 1
    }
    if [[ "$DOCTOR_ONLY" == "1" ]]; then
        cat /tmp/cmux-linux-doctor.txt
        exit 0
    fi
fi

CARGO_ARGS=(--manifest-path linux/Cargo.toml -p cmux-linux)
if [[ "$RELEASE" == "1" ]]; then
    CARGO_ARGS+=(--release)
fi

if [[ "$RESET" == "1" ]]; then
    SESSION_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/cmux"
    rm -f "$SESSION_DIR/cmux-linux-session.json"
    echo "Session state cleared."
fi

exec cargo run "${CARGO_ARGS[@]}" -- "$@"
