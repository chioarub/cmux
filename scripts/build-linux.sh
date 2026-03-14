#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_DIR"

RELEASE=0
INSTALL=0

usage() {
    cat <<'EOF'
Usage: ./scripts/build-linux.sh [--release] [--install]

Build the cmux Linux app.

Options:
  --release    Build with optimizations (default: debug)
  --install    Copy binary to ~/.local/bin and install desktop entry
  -h, --help   Show this help
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release) RELEASE=1; shift ;;
        --install) INSTALL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1"; usage; exit 1 ;;
    esac
done

if [[ "$RELEASE" == "1" ]]; then
    echo "Building cmux-linux (release)..."
    cargo build --manifest-path linux/Cargo.toml -p cmux-linux --release
    BINARY="linux/target/release/cmux-linux"
else
    echo "Building cmux-linux (debug)..."
    cargo build --manifest-path linux/Cargo.toml -p cmux-linux
    BINARY="linux/target/debug/cmux-linux"
fi

echo "Built: $BINARY"

if [[ "$INSTALL" == "1" ]]; then
    mkdir -p ~/.local/bin
    cp "$BINARY" ~/.local/bin/cmux-linux
    echo "Installed binary to ~/.local/bin/cmux-linux"

    mkdir -p ~/.local/share/applications
    cp linux/cmux-linux.desktop ~/.local/share/applications/cmux-linux.desktop
    echo "Installed desktop entry to ~/.local/share/applications/cmux-linux.desktop"

    if command -v update-desktop-database &>/dev/null; then
        update-desktop-database ~/.local/share/applications 2>/dev/null || true
    fi

    echo ""
    echo "cmux-linux installed. Run with: cmux-linux"
fi
