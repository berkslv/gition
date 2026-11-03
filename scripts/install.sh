#!/bin/sh
# Installs gition from GitHub Releases.
#   curl -fsSL https://raw.githubusercontent.com/berkslv/gition/main/scripts/install.sh | sh
# Env: GITION_VERSION (e.g. v0.1.0, default latest), GITION_INSTALL_DIR (default ~/.local/bin)
set -eu

REPO="berkslv/gition"
INSTALL_DIR="${GITION_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'gition: %s\n' "$*"; }
die() { printf 'gition: error: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required"; }

need curl
need tar
need uname

case "$(uname -s)" in
  Linux) os="unknown-linux-gnu" ;;
  Darwin) os="apple-darwin" ;;
  MINGW* | MSYS* | CYGWIN*) die "on Windows use: powershell -c \"irm https://raw.githubusercontent.com/$REPO/main/scripts/install.ps1 | iex\"" ;;
  *) die "unsupported OS: $(uname -s)" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch="x86_64" ;;
  arm64 | aarch64) arch="aarch64" ;;
  *) die "unsupported architecture: $(uname -m)" ;;
esac
target="$arch-$os"

version="${GITION_VERSION:-}"
if [ -z "$version" ]; then
  # /releases/latest redirects to /releases/tag/<version>.
  url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")
  version="${url##*/}"
  case "$version" in v*) ;; *) die "could not determine the latest version" ;; esac
fi

name="gition-$version-$target"
base="https://github.com/$REPO/releases/download/$version"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

say "downloading $name"
curl -fsSL "$base/$name.tar.gz" -o "$tmp/$name.tar.gz" || die "no release asset for $target ($version)"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"

expected=$(grep " $name.tar.gz\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)
[ -n "$expected" ] || die "no checksum for $name.tar.gz"
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)
else
  actual=$(shasum -a 256 "$tmp/$name.tar.gz" | cut -d' ' -f1)
fi
[ "$expected" = "$actual" ] || die "checksum mismatch for $name.tar.gz"

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/$name/gition" "$INSTALL_DIR/gition" 2>/dev/null || {
  cp "$tmp/$name/gition" "$INSTALL_DIR/gition"
  chmod 755 "$INSTALL_DIR/gition"
}
say "installed $version to $INSTALL_DIR/gition"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "add $INSTALL_DIR to your PATH, e.g.: echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.profile" ;;
esac
