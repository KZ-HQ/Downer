#!/usr/bin/env bash
#
# Install the FFmpeg used by `make extension-e2e-native`.
#
# The native-download end-to-end test drives a real HLS download, which needs
# FFmpeg 7.1 or newer: below that, `-allowed_segment_extensions` and
# `-extension_picky` do not exist and every HLS download dies at argument
# parsing (KEI-81). Ubuntu 24.04 ships 6.1.1, so a cloud session or a CI runner
# has no usable FFmpeg even though it has one on PATH.
#
# This is the companion of `install_test_browser.sh` and works the same way and
# for the same reason: conda-forge is one host that a network-restricted sandbox
# generally allows, and it serves a current FFmpeg. It deliberately does not
# touch the system FFmpeg — the install lands beside the browser and the tests
# look for it there.
#
# The script is idempotent: it does nothing when a new enough FFmpeg is already
# installed. See docs/e2e-firefox.md.

set -euo pipefail

PREFIX="${DOWNER_BROWSER_PREFIX:-/opt/downer-browser}"
MICROMAMBA_VERSION="${MICROMAMBA_VERSION:-2.9.0}"
FFMPEG_SPEC="${FFMPEG_SPEC:-ffmpeg>=7.1}"
CHANNEL_BASE="https://conda.anaconda.org/conda-forge"

ENV_DIR="$PREFIX/ffmpeg"
MAMBA_DIR="$PREFIX/mamba"
MICROMAMBA="$MAMBA_DIR/bin/micromamba"

if [ -x "$PREFIX/bin/ffmpeg" ]; then
  echo "Test FFmpeg already installed in $PREFIX"
  "$PREFIX/bin/ffmpeg" -version | sed -n '1p'
  exit 0
fi

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) subdir="linux-64" ;;
  Linux-aarch64) subdir="linux-aarch64" ;;
  Darwin-x86_64) subdir="osx-64" ;;
  Darwin-arm64) subdir="osx-arm64" ;;
  *)
    echo "error: unsupported platform $(uname -s)-$(uname -m)" >&2
    exit 1
    ;;
esac

mkdir -p "$MAMBA_DIR" "$PREFIX/bin"

# Shared with install_test_browser.sh: whichever runs first bootstraps it.
if [ ! -x "$MICROMAMBA" ]; then
  echo "Downloading micromamba $MICROMAMBA_VERSION ($subdir)"
  tarball="$(mktemp -t micromamba-XXXXXX.tar.bz2)"
  trap 'rm -f "$tarball"' EXIT
  curl -fsSL --retry 3 --retry-delay 2 \
    "$CHANNEL_BASE/$subdir/micromamba-$MICROMAMBA_VERSION-0.tar.bz2" -o "$tarball"
  tar -xjf "$tarball" -C "$MAMBA_DIR" bin/micromamba
  chmod +x "$MICROMAMBA"
fi

echo "Installing $FFMPEG_SPEC into $ENV_DIR"
MAMBA_ROOT_PREFIX="$MAMBA_DIR" "$MICROMAMBA" create --yes --quiet \
  --prefix "$ENV_DIR" \
  --channel conda-forge --override-channels \
  "$FFMPEG_SPEC"

MAMBA_ROOT_PREFIX="$MAMBA_DIR" "$MICROMAMBA" clean --yes --tarballs >/dev/null

ln -sf "$ENV_DIR/bin/ffmpeg" "$PREFIX/bin/ffmpeg"
ln -sf "$ENV_DIR/bin/ffprobe" "$PREFIX/bin/ffprobe"

echo
"$PREFIX/bin/ffmpeg" -version | sed -n '1p'
echo
echo "Installed in $PREFIX. The native end-to-end test finds it there by default."
