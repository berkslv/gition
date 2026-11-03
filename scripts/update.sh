#!/bin/sh
# Updates gition to the latest release, where it's installed now.
#   curl -fsSL https://raw.githubusercontent.com/berkslv/gition/main/scripts/update.sh | sh
set -eu

REPO="berkslv/gition"
RAW="https://raw.githubusercontent.com/$REPO/main/scripts"

say() { printf 'gition: %s\n' "$*"; }
die() { printf 'gition: error: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || die "'curl' is required"
bin=$(command -v gition 2>/dev/null) || die "gition is not on your PATH; install it with: curl -fsSL $RAW/install.sh | sh"
dir=$(dirname "$bin")

# /releases/latest redirects to /releases/tag/<version>.
url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")
latest="${url##*/}"
case "$latest" in v*) ;; *) die "could not determine the latest version" ;; esac

current="v$(gition --version 2>/dev/null | cut -d' ' -f2)"
if [ "$current" = "$latest" ]; then
  say "$current is already the latest version"
  exit 0
fi
case "$dir" in
  */.cargo/bin) die "gition was installed with cargo; update it with: cargo install gition" ;;
esac
[ -w "$dir" ] || die "can't write to $dir; run: curl -fsSL $RAW/update.sh | sudo sh"

say "updating $current to $latest in $dir"
curl -fsSL "$RAW/install.sh" | GITION_INSTALL_DIR="$dir" GITION_VERSION="$latest" sh
