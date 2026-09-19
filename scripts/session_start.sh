#!/usr/bin/env bash
#
# SessionStart hook: give a Claude Code cloud session the end-to-end browser.
#
# A hook in `.claude/settings.json` runs wherever Claude Code runs, including
# on a contributor's own machine, so this script's whole job is to decide
# whether it is somewhere that installing a browser is expected. It is not a
# convenience wrapper: `make extension-browser` is the way to install one
# deliberately, on any machine, and it calls the installer directly.
#
# The guard is `CLAUDE_CODE_REMOTE`, which a cloud session VM sets to `true`
# and which is never `true` locally. Anywhere else this exits 0 in silence,
# before touching the filesystem. `DOWNER_SKIP_BROWSER_INSTALL=1` turns it off
# in a cloud session too.
#
# Nothing here writes outside "$DOWNER_BROWSER_PREFIX" (default
# /opt/downer-browser): the installer touches no system package manager and no
# file in the repository. See docs/e2e-firefox.md.

set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

if [ "${DOWNER_SKIP_BROWSER_INSTALL:-}" = "1" ]; then
  exit 0
fi

# `$CLAUDE_PROJECT_DIR` is the repository root whatever the session's working
# directory is; the fallback keeps the script runnable by hand.
root="${CLAUDE_PROJECT_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"

# A cloud session must not fail to start because a browser did not install, so
# the failure is reported and swallowed. `make extension-e2e` says the same
# thing later, at the point where it actually matters.
if ! "$root/scripts/install_test_browser.sh"; then
  echo "note: could not install the end-to-end browser; run 'make extension-browser'" >&2
fi
