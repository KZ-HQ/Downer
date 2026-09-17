.DEFAULT_GOAL := help

CARGO ?= cargo
FFMPEG ?= ffmpeg
NPM ?= npm

.PHONY: help setup doctor build fmt fmt-check lint test version-check extension-deps extension-lint extension-test extension-check check run install extension extension-package extension-install fixture-site fixture-site-peer clean

help: ## Show available commands
	@awk 'BEGIN {FS = ":.*##"; printf "Usage: make <target>\n\nTargets:\n"} /^[a-zA-Z0-9_-]+:.*##/ {printf "  %-12s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

setup: ## Install Rust and FFmpeg with Homebrew when available
	@if command -v brew >/dev/null 2>&1; then \
		if ! brew list --formula rust >/dev/null 2>&1; then brew install rust; fi; \
		if ! brew list --formula ffmpeg >/dev/null 2>&1; then brew install ffmpeg; fi; \
	else \
		echo "Homebrew not found; using any Rust and FFmpeg installations already on PATH."; \
	fi
	@$(MAKE) doctor

doctor: ## Verify Rust and FFmpeg are available
	@command -v $(CARGO) >/dev/null 2>&1 || { echo "error: cargo is not available" >&2; exit 1; }
	@command -v $(FFMPEG) >/dev/null 2>&1 || { echo "error: ffmpeg is not available" >&2; exit 1; }
	@$(CARGO) --version
	@$(FFMPEG) -version | sed -n '1p'

build: ## Build the debug binary
	$(CARGO) build

fmt: ## Format Rust sources
	$(CARGO) fmt --all

fmt-check: ## Check Rust formatting without changing files
	$(CARGO) fmt --all -- --check

lint: ## Run Clippy with warnings treated as errors
	$(CARGO) clippy --all-targets --all-features -- -D warnings

test: ## Run unit, integration, and doc tests
	$(CARGO) test

version-check: ## Verify the Cargo package and extension manifest versions agree
	@python3 scripts/check_versions.py

extension-deps: node_modules/.install-stamp ## Install development-only extension tooling

# Reinstall whenever the manifest or the lockfile changes.
#
# Testing for one installed executable is not enough. A dependency with no
# executable — jsdom — is invisible to such a test, so adding one left every
# existing checkout short-circuiting the guard and failing later with
# "Cannot find module", while CI stayed green because it always starts from an
# empty node_modules. The stamp depends on the files that decide what should be
# installed, so make reinstalls exactly when they change.
node_modules/.install-stamp: package.json package-lock.json
	$(NPM) install --no-audit --no-fund
	@touch $@

extension-lint: extension-deps ## Lint the Firefox extension with web-ext
	@node_modules/.bin/web-ext lint --source-dir extension --warnings-as-errors

extension-test: extension-deps ## Run the extension's Node unit tests
	@node --test tests/extension/*.test.js

extension-check: extension-lint extension-test ## Validate Firefox extension JSON and JavaScript
	@python3 -m json.tool extension/manifest.json >/dev/null
	@node --check extension/background.js
	@node --check extension/hls.js
	@node --check extension/task-protocol.js
	@node --check extension/content.js
	@node --check extension/media-scan.js
	@node --check extension/options.js
	@node --check extension/job-state.js
	@node --check extension/job-view.js
	@node --check extension/popup.js
	@echo "Firefox extension files are valid"

extension: extension-install extension-package ## Build/register the native host and package the extension

check: fmt-check lint version-check test extension-check ## Run Rust and Firefox extension checks

FIXTURE_SITE := python3 tests/fixtures/protected_site.py

fixture-site: ## Serve cookie-gated media on localhost:8080 for manual verification
	$(FIXTURE_SITE) --host localhost --port 8080 --peer http://127.0.0.1:8081 --ffmpeg $(FFMPEG)

fixture-site-peer: ## Second fixture origin on 127.0.0.1:8081, for the cross-host cases
	$(FIXTURE_SITE) --host 127.0.0.1 --port 8081 --peer http://localhost:8080 --ffmpeg $(FFMPEG)

run: ## Run downer; pass CLI arguments with ARGS="..."
	@test -n "$(ARGS)" || { echo 'usage: make run ARGS="URL [options]"' >&2; exit 2; }
	$(CARGO) run -- $(ARGS)

install: ## Install downer with Cargo
	$(CARGO) install --path .

extension-package: extension-check ## Package the Firefox extension as a ZIP
	@mkdir -p dist
	@rm -f dist/downer-firefox.zip
	@cd extension && zip -qr ../dist/downer-firefox.zip .
	@echo "Created dist/downer-firefox.zip"

extension-install: ## Build and register the Firefox native messaging host
	./scripts/install_native_host.sh

clean: ## Remove Cargo build artifacts
	$(CARGO) clean
