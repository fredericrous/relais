# Homebrew's cargo is usually first on PATH and IGNORES rust-toolchain.toml,
# because the pin is a rustup feature rather than a cargo one. `make check`
# then lints with a different clippy than CI does, a clean local run means
# nothing, and the first you hear of it is a red build. Prefer rustup's
# shim, which reads the pin.
CARGO := $(shell command -v rustup >/dev/null 2>&1 && test -x "$(HOME)/.cargo/bin/cargo" && echo "$(HOME)/.cargo/bin/cargo" || echo cargo)

.PHONY: all check lint test build fmt toolchain msrv

all: check

## Everything CI runs, in the order it runs it. No `deps` target: the
## std-only gate the sibling repos carry does not apply here (SPEC §13).
check: toolchain lint test msrv

## Say which toolchain is about to be used, so a mismatch is visible.
toolchain:
	@echo "using: $$($(CARGO) --version)  (pinned: $$(sed -n 's/^channel = \"\(.*\)\"/\1/p' rust-toolchain.toml))"

lint:
	$(CARGO) fmt --all --check
	$(CARGO) clippy --all-targets -- -D warnings

test:
	$(CARGO) test

## The floor `rust-version` claims, actually compiled against. Both
## spellings are tried because a false SKIP is worse than no target at all.
msrv:
	@v=$$(sed -n 's/^[[:space:]]*rust-version = \"\(.*\)\"/\1/p' Cargo.toml); \
	for t in $$v $$v.0; do \
		if rustup run $$t cargo --version >/dev/null 2>&1; then \
			echo "msrv: building with $$t"; exec rustup run $$t cargo build; \
		fi; \
	done; \
	echo "msrv: rust $$v is not installed — run 'rustup toolchain install $$v'"; \
	echo "msrv: SKIPPED, so this run does not prove the floor"

build:
	$(CARGO) build --release

fmt:
	$(CARGO) fmt --all
