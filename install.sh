#!/bin/sh
# Install ZenCode CLI from source on Linux and macOS.
#
# One command (inspect the script first if you want to):
#   curl -fsSL https://raw.githubusercontent.com/7enlabs/zencode/main/install.sh | sh
#
# What this script does, and nothing else:
#   1. Clones the repo, or fast-forwards it if it already exists.
#   2. Builds the CLI crate in release mode with cargo.
#   3. Copies the resulting `zen` binary to the install directory.
# It never uses sudo, never modifies your shell profile, and never
# installs a Rust toolchain for you. If cargo is missing, it stops
# and tells you how to install Rust.
#
# Environment overrides, all optional:
#   ZENCODE_REPO        Git URL to clone (default: the 7enlabs repo below).
#   ZENCODE_BRANCH      Branch to build (default: main).
#   ZENCODE_SRC_DIR     Where the repo is cloned (default: ~/.local/share/zencode).
#   ZENCODE_INSTALL_DIR Where the `zen` binary is copied (default: ~/.local/bin).
set -eu

REPO="${ZENCODE_REPO:-https://github.com/7enlabs/zencode.git}"
BRANCH="${ZENCODE_BRANCH:-main}"
SRC_DIR="${ZENCODE_SRC_DIR:-$HOME/.local/share/zencode}"
INSTALL_DIR="${ZENCODE_INSTALL_DIR:-$HOME/.local/bin}"

need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: required command not found: $1" >&2
        exit 1
    fi
}

need git

if command -v cargo >/dev/null 2>&1; then
    CARGO="cargo"
elif [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO="$HOME/.cargo/bin/cargo"
else
    echo "error: cargo not found." >&2
    echo "Install Rust first, for example with rustup, then rerun this script:" >&2
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
    exit 1
fi

if [ -d "$SRC_DIR/.git" ]; then
    echo "Updating existing checkout in $SRC_DIR"
    git -C "$SRC_DIR" fetch origin "$BRANCH"
    git -C "$SRC_DIR" checkout "$BRANCH"
    git -C "$SRC_DIR" merge --ff-only "origin/$BRANCH"
else
    echo "Cloning $REPO ($BRANCH) into $SRC_DIR"
    git clone --branch "$BRANCH" --depth 1 "$REPO" "$SRC_DIR"
fi

echo "Building zen-cli in release mode (this can take a few minutes)"
"$CARGO" build --release -p zen-cli --manifest-path "$SRC_DIR/Cargo.toml"

BIN="$SRC_DIR/target/release/zen"
if [ ! -x "$BIN" ]; then
    echo "error: build finished but $BIN is missing or not executable" >&2
    exit 1
fi

mkdir -p "$INSTALL_DIR"
cp "$BIN" "$INSTALL_DIR/zen"
chmod +x "$INSTALL_DIR/zen"

echo "Installed zen to $INSTALL_DIR/zen"
if ! "$INSTALL_DIR/zen" --help >/dev/null 2>&1; then
    echo "warning: the installed binary did not respond to --help; run it directly to check it" >&2
fi

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        echo "Add the install directory to your PATH, for example:"
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac
echo "Run: zen"
