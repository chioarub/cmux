#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_DIR"

DOCTOR_ONLY=0

usage() {
    cat <<'EOF'
Usage: ./scripts/run-linux.sh [--doctor] [-- <binary-args>]

Options:
  --doctor            Print the Linux/Wayland readiness report and exit
  -h, --help          Show this help

Socket:
  Defaults to /tmp/cmux-linux.sock
  Override with CMUX_SOCKET or CMUX_SOCKET_PATH
EOF
}

if [[ "${1-}" == "-h" || "${1-}" == "--help" ]]; then
    usage
    exit 0
fi

while [[ $# -gt 0 ]]; do
    case "$1" in
        --backend)
            echo "error: Linux now supports only the built-in VTE backend; --backend has been removed"
            exit 2
            ;;
        --doctor)
            DOCTOR_ONLY=1
            shift
            ;;
        --)
            shift
            break
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            break
            ;;
    esac
done

python3 "$SCRIPT_DIR/linux_wayland_doctor.py" >/tmp/cmux-linux-doctor.txt || {
    cat /tmp/cmux-linux-doctor.txt
    exit 1
}
cat /tmp/cmux-linux-doctor.txt

if [[ "$DOCTOR_ONLY" == "1" ]]; then
    exit 0
fi

export GDK_BACKEND=wayland
exec cargo run --manifest-path linux/Cargo.toml -p cmux-linux "$@"
