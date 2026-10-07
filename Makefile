# Homebrew's cargo is usually first on PATH and IGNORES rust-toolchain.toml,
# because the pin is a rustup feature rather than a cargo one. `make check`
# then lints with a different clippy than CI does, a clean local run means
# nothing, and the first you hear of it is a red build. Prefer rustup's
# shim, which reads the pin.
CARGO := $(shell command -v rustup >/dev/null 2>&1 && test -x "$(HOME)/.cargo/bin/cargo" && echo "$(HOME)/.cargo/bin/cargo" || echo cargo)

.PHONY: all check lint test build fmt toolchain msrv audit plugin

all: check

## Everything CI runs, in the order it runs it. No `deps` target: the
## std-only gate the sibling repos carry does not apply here (SPEC §13).
## `lint` ends with the architecture gates, which the compiler cannot
## state: a cycle between two modules of one crate compiles happily.
##
## All five gates, and green means all five RAN: `msrv`, `audit` and `plugin`
## fail rather than skipping unless MSRV_SKIP_OK / AUDIT_SKIP_OK /
## PLUGIN_SKIP_OK say otherwise, because a target that exits 0 on "I could
## not check" is worse than no target at all.
check: toolchain lint test msrv audit plugin

## Say which toolchain is about to be used, so a mismatch is visible.
toolchain:
	@echo "using: $$($(CARGO) --version)  (pinned: $$(sed -n 's/^channel = \"\(.*\)\"/\1/p' rust-toolchain.toml))"

lint:
	$(CARGO) fmt --all --check
	$(CARGO) clippy --all-targets -- -D warnings
	python3 scripts/check-module-cycles.py

test:
	$(CARGO) test

## The floor `rust-version` claims, actually compiled against. Both
## spellings are tried because a false SKIP is worse than no target at all
## — and an absent toolchain now FAILS, so a green `make check` means the
## floor was really compiled. `MSRV_SKIP_OK=1` opts out explicitly, for a
## machine that deliberately carries one toolchain.
msrv:
	@v=$$(sed -n 's/^[[:space:]]*rust-version = \"\(.*\)\"/\1/p' Cargo.toml); \
	for t in $$v $$v.0; do \
		if rustup run $$t cargo --version >/dev/null 2>&1; then \
			echo "msrv: building with $$t"; exec rustup run $$t cargo build; \
		fi; \
	done; \
	echo "msrv: rust $$v is not installed — run 'rustup toolchain install $$v'"; \
	if [ -n "$$MSRV_SKIP_OK" ]; then \
		echo "msrv: SKIPPED by MSRV_SKIP_OK, so this run does not prove the floor"; \
		exit 0; \
	fi; \
	echo "msrv: FAILED — the floor was not compiled (set MSRV_SKIP_OK=1 to skip anyway)"; \
	exit 1

## `cargo audit` over the locked tree, so `check` covers the fourth CI
## job too. Advisory in the same way CI's job is: an advisory against a
## dependency is information about the ecosystem, not a defect in the
## change under review, so findings are printed loudly and do not fail
## here. An audit that could not RUN is a different thing and does fail,
## because an unchecked tree looks exactly like a clean one.
## `AUDIT_SKIP_OK=1` skips it instead of installing the tool.
audit:
	@if ! command -v cargo-audit > /dev/null 2>&1; then \
		if [ -n "$$AUDIT_SKIP_OK" ]; then \
			echo "audit: SKIPPED by AUDIT_SKIP_OK — install it with 'cargo install --locked cargo-audit'"; \
			exit 0; \
		fi; \
		echo "audit: cargo-audit is not installed; installing it ('cargo install --locked cargo-audit'), or set AUDIT_SKIP_OK=1 to skip"; \
		$(CARGO) install --locked cargo-audit || { \
			echo "audit: cargo-audit could not be installed — the tree was NOT checked"; \
			exit 1; \
		}; \
	fi; \
	$(CARGO) audit --color never || \
		echo "audit: advisories above. Advisory here and in CI; the RELEASE workflow blocks on a vulnerability."

## The Claude Code the relais plugin (claude-plugin/) is tested on: the
## range its manifest names, and the exact version CI installs. Raising the
## range means re-running the plugin's probes first (plan: all-native-mod §9).
CLAUDE_CODE_MIN := 2.1.291
CLAUDE_CODE_MAX := 2.2.0
CLAUDE_CODE_PIN := 2.1.291

## The relais Claude Code plugin: `claude plugin validate` reads the manifest
## and the hooks module the way the engine will, `claude plugin test` runs
## its tests (no sign-in, no network). A missing `claude` FAILS rather than
## skips, as `msrv` and `audit` do; `PLUGIN_SKIP_OK=1` opts out explicitly.
plugin:
	@if ! command -v claude > /dev/null 2>&1; then \
		echo "plugin: claude is not on PATH — install it with 'npm i -g @anthropic-ai/claude-code@$(CLAUDE_CODE_PIN)'"; \
		if [ -n "$$PLUGIN_SKIP_OK" ]; then \
			echo "plugin: SKIPPED by PLUGIN_SKIP_OK, so this run does not prove the plugin"; \
			exit 0; \
		fi; \
		echo "plugin: FAILED — the plugin was not checked (set PLUGIN_SKIP_OK=1 to skip anyway)"; \
		exit 1; \
	fi; \
	echo "plugin: claude $$(claude --version) (plugin tested on >= $(CLAUDE_CODE_MIN) < $(CLAUDE_CODE_MAX); CI pins $(CLAUDE_CODE_PIN))"
	claude plugin validate claude-plugin
	cd claude-plugin && claude plugin test

build:
	$(CARGO) build --release

fmt:
	$(CARGO) fmt --all
