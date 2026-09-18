#!/usr/bin/env bash
#
# Install the Firefox and geckodriver used by `make extension-e2e`.
#
# The end-to-end tests need a real Firefox, and the usual ways of getting one
# are not available everywhere the tests should run. Ubuntu's `firefox` package
# is a snap stub that needs snapd, Mozilla's own downloads and the geckodriver
# releases on GitHub are unreachable from a network-restricted CI or agent
# sandbox, and Playwright's Firefox cannot install extensions at all.
#
# conda-forge repackages Mozilla's official build, and ships geckodriver beside
# it, from a single host (conda.anaconda.org) that such sandboxes generally do
# allow. That is the only reason conda appears in a project that otherwise has
# no Python dependencies at runtime; nothing outside these tests uses it.
#
# The script is idempotent: it does nothing when the browser is already there.
# See docs/e2e-firefox.md.

set -euo pipefail

PREFIX="${DOWNER_BROWSER_PREFIX:-/opt/downer-browser}"
MICROMAMBA_VERSION="${MICROMAMBA_VERSION:-2.9.0}"
FIREFOX_SPEC="${FIREFOX_SPEC:-firefox}"
GECKODRIVER_SPEC="${GECKODRIVER_SPEC:-geckodriver}"
CHANNEL_BASE="https://conda.anaconda.org/conda-forge"

ENV_DIR="$PREFIX/env"
MAMBA_DIR="$PREFIX/mamba"
MICROMAMBA="$MAMBA_DIR/bin/micromamba"

if [ -x "$PREFIX/bin/firefox" ] && [ -x "$PREFIX/bin/geckodriver" ]; then
  echo "Test browser already installed in $PREFIX"
  "$PREFIX/bin/firefox" --version
  "$PREFIX/bin/geckodriver" --version | sed -n '1p'
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

if [ ! -x "$MICROMAMBA" ]; then
  echo "Downloading micromamba $MICROMAMBA_VERSION ($subdir)"
  tarball="$(mktemp -t micromamba-XXXXXX.tar.bz2)"
  trap 'rm -f "$tarball"' EXIT
  curl -fsSL --retry 3 --retry-delay 2 \
    "$CHANNEL_BASE/$subdir/micromamba-$MICROMAMBA_VERSION-0.tar.bz2" -o "$tarball"
  tar -xjf "$tarball" -C "$MAMBA_DIR" bin/micromamba
  chmod +x "$MICROMAMBA"
fi

echo "Installing $FIREFOX_SPEC and $GECKODRIVER_SPEC into $ENV_DIR"
# `--override-channels` keeps the resolve on conda-forge alone, so the install
# needs exactly one host however the machine's own conda is configured.
MAMBA_ROOT_PREFIX="$MAMBA_DIR" "$MICROMAMBA" create --yes --quiet \
  --prefix "$ENV_DIR" \
  --channel conda-forge --override-channels \
  "$FIREFOX_SPEC" "$GECKODRIVER_SPEC"

MAMBA_ROOT_PREFIX="$MAMBA_DIR" "$MICROMAMBA" clean --yes --tarballs >/dev/null

ln -sf "$ENV_DIR/bin/firefox" "$PREFIX/bin/firefox"
ln -sf "$ENV_DIR/bin/geckodriver" "$PREFIX/bin/geckodriver"

echo
"$PREFIX/bin/firefox" --version
"$PREFIX/bin/geckodriver" --version | sed -n '1p'
echo
echo "Installed in $PREFIX. The tests find it there by default; elsewhere set"
echo "  export DOWNER_BROWSER_PREFIX=$PREFIX"
