#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script must be run on macOS." >&2
  exit 1
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v cargo >/dev/null || { echo "Rust/Cargo is required." >&2; exit 1; }
command -v wails >/dev/null || { echo "Wails v2 CLI is required." >&2; exit 1; }

# Wails v2 produces an architecture-native .app. Match the Rust Engine
# architecture to the host rather than assuming Intel or Apple Silicon.
cargo build --locked --release -p df-engine
wails build -clean

APP="$ROOT/build/bin/datafactory-studio.app"
MACOS_DIR="$APP/Contents/MacOS"
if [[ ! -d "$MACOS_DIR" ]]; then
  echo "Expected Wails v2 bundle at $APP (missing Contents/MacOS)." >&2
  exit 1
fi

# Keep the Rust executable next to the Wails executable so Studio can find it
# without DF_ENGINE_PATH when launched from Finder.
install -m 755 "$ROOT/target/release/df-engine" "$MACOS_DIR/df-engine"

# Local ad-hoc signing after adding the sidecar keeps the bundle internally
# consistent on Apple Silicon. This is NOT Developer ID signing/notarization.
codesign --force --sign - "$MACOS_DIR/df-engine"
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"

echo "Packaged: $APP"
echo "Rust Engine: $MACOS_DIR/df-engine"
echo "Architecture: $(uname -m)"
echo "Local build only. Distribution outside your machine requires proper Developer ID signing/notarization."
