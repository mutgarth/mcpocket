#!/usr/bin/env bash
set -euo pipefail

MCPOCKET_REPO="https://github.com/mutgarth/mcpocket"
INSTALL_DIR="${MCPOCKET_INSTALL_DIR:-$HOME/.local/bin}"
BIN_PATH="$INSTALL_DIR/mcpocket"

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS-$ARCH" in
  Darwin-arm64)   PLATFORM="macos-arm64" ;;
  Darwin-x86_64)  PLATFORM="macos-x86_64" ;;
  Linux-x86_64)   PLATFORM="linux-x86_64" ;;
  *)
    echo "Unsupported platform: $OS-$ARCH"
    exit 1
    ;;
esac

echo ""
echo "Installing mcpocket for $PLATFORM"
echo "──────────────────────────────────────"

mkdir -p "$INSTALL_DIR"

TMP_BIN="$(mktemp "${TMPDIR:-/tmp}/mcpocket.XXXXXX")"
cleanup() {
  rm -f "$TMP_BIN"
}
trap cleanup EXIT

echo "→ Downloading latest mcpocket release..."
curl -fsSL --progress-bar \
  "$MCPOCKET_REPO/releases/latest/download/mcpocket-$PLATFORM" \
  -o "$TMP_BIN"

chmod +x "$TMP_BIN"
mv "$TMP_BIN" "$BIN_PATH"

echo "→ Installed $BIN_PATH"

PROFILE=""
if [ -f "$HOME/.zshrc" ]; then PROFILE="$HOME/.zshrc"
elif [ -f "$HOME/.bashrc" ]; then PROFILE="$HOME/.bashrc"
elif [ -f "$HOME/.bash_profile" ]; then PROFILE="$HOME/.bash_profile"
fi

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    if [ -n "$PROFILE" ] && ! grep -q "$INSTALL_DIR" "$PROFILE"; then
      printf '\nexport PATH="%s:$PATH"\n' "$INSTALL_DIR" >> "$PROFILE"
      echo "→ Added $INSTALL_DIR to PATH in $PROFILE"
    else
      echo "→ Add $INSTALL_DIR to PATH if your shell cannot find mcpocket"
    fi
    export PATH="$INSTALL_DIR:$PATH"
    ;;
esac

echo ""
echo "──────────────────────────────────────"
mcpocket --help >/dev/null
echo "mcpocket is installed."
echo ""
echo "Next:"
echo "  mcpocket doctor"
echo "  mcpocket list"
echo "  mcpocket sync --gateway --to claude,codex,opencode"
