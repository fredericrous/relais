# Homebrew's cargo is usually first on PATH and IGNORES rust-toolchain.toml,
# because the pin is a rustup feature rather than a cargo one. `make check`
# then lints with a different clippy than CI does, a clean local run means
# nothing, and the first you hear of it is a red build. Prefer rustup's
# shim, which reads the pin.
CARGO := $(shell command -v rustup >/dev/null 2>&1 && test -x "$(HOME)/.cargo/bin/cargo" && echo "$(HOME)/.cargo/bin/cargo" || echo cargo)

.PHONY: all check lint test build fmt toolchain msrv audit probe-hooks

all: check

## Everything CI runs, in the order it runs it. No `deps` target: the
## std-only gate the sibling repos carry does not apply here (SPEC §13).
## `lint` ends with the architecture gates, which the compiler cannot
## state: a cycle between two modules of one crate compiles happily.
##
## All four gates, and green means all four RAN: `msrv` and `audit` fail
## rather than skipping unless MSRV_SKIP_OK / AUDIT_SKIP_OK say otherwise,
## because a target that exits 0 on "I could not check" is worse than no
## target at all.
check: toolchain lint test msrv audit

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

build:
	$(CARGO) build --release

fmt:
	$(CARGO) fmt --all

## `relais doctor --probe-hooks`, built first so a stale binary is never
## what runs. Its own target, never folded into `check`: it needs a real
## Claude Code on PATH, costs money (a live model session) and touches
## the network, none of which `check` may depend on to stay green on a
## machine with neither.
probe-hooks:
	$(CARGO) build
	$(CARGO) run -- doctor --probe-hooks
