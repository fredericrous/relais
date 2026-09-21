#!/bin/sh
# Install the relais binary. Nothing else.
#
#   curl -fsSL https://raw.githubusercontent.com/fredericrous/relais/main/install/install.sh | sh
#
# It downloads a checksum-verified binary, puts it on your PATH, and tells you
# what to run next. Nothing else: it writes no policy, grants no trust and
# starts no coordinator. That restraint is the point — relais launches
# models with your credentials, and an installer that armed anything on first
# contact with your machine would contradict the guarantee on its way in.
#
# POSIX sh, no bashisms: this has to run wherever the rest of the toolchain
# does, including a minimal CI image with no bash.
set -eu

REPO="fredericrous/relais"
# `$HOME/.local/bin` by default: the conventional per-user location, already on
# PATH in most shells, and writable without sudo.
BIN_DIR="${RELAIS_BIN_DIR:-$HOME/.local/bin}"
VERSION="${RELAIS_VERSION:-latest}"

RED='\033[31m'; GREEN='\033[32m'; YELLOW='\033[33m'; OFF='\033[0m'
if [ -n "${NO_COLOR:-}" ] || [ ! -t 1 ]; then RED=''; GREEN=''; YELLOW=''; OFF=''; fi

say()  { printf '  %s\n' "$1"; }
ok()   { printf "  ${GREEN}✓${OFF} %s\n" "$1"; }
warn() { printf "  ${YELLOW}!${OFF} %s\n" "$1"; }
die()  { printf "  ${RED}✗${OFF} %s\n" "$1" >&2; exit 1; }

need() { command -v "$1" > /dev/null 2>&1 || die "$1 is required and was not found"; }

need uname
need tar

# curl or wget, whichever is here.
if command -v curl > /dev/null 2>&1; then
    fetch() { curl -fsSL "$1"; }
    fetch_to() { curl -fsSL "$1" -o "$2"; }
elif command -v wget > /dev/null 2>&1; then
    fetch() { wget -qO- "$1"; }
    fetch_to() { wget -qO "$2" "$1"; }
else
    die "neither curl nor wget is available"
fi

target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux)
            # musl when there is no glibc: the static build runs on distros
            # older than whatever the release was built against, which is the
            # usual reason a "linux" binary fails for somebody.
            if ldd --version 2>&1 | grep -qi musl; then libc=musl; else libc=gnu; fi
            case "$arch" in
                x86_64|amd64)  echo "x86_64-unknown-linux-$libc" ;;
                aarch64|arm64) [ "$libc" = "musl" ] && die "no aarch64 musl build yet — build from source with cargo install --git https://github.com/fredericrous/relais"
                               echo "aarch64-unknown-linux-gnu" ;;
                *) die "unsupported architecture: $arch" ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                x86_64) echo "x86_64-apple-darwin" ;;
                arm64)  echo "aarch64-apple-darwin" ;;
                *) die "unsupported architecture: $arch" ;;
            esac
            ;;
        MINGW*|MSYS*|CYGWIN*)
            # Reachable: this runs under Git Bash, which every Git for Windows
            # install ships. Point at the PowerShell installer rather than the
            # releases page, because there IS a one-liner for this platform.
            die "on Windows, use PowerShell:
    irm https://raw.githubusercontent.com/$REPO/main/install/install.ps1 | iex
  or download the .zip from https://github.com/$REPO/releases"
            ;;
        *) die "unsupported OS: $os" ;;
    esac
}

resolve_version() {
    if [ "$VERSION" != "latest" ]; then
        echo "${VERSION#v}"
        return
    fi
    # The API rather than the /releases/latest redirect, so a rate-limited or
    # offline run fails LOUDLY here instead of downloading a 404 page and
    # handing you a tarball full of HTML.
    tag=$(fetch "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$tag" ] || die "could not determine the latest release (rate limited? set RELAIS_VERSION=vX.Y.Z)"
    echo "${tag#v}"
}

main() {
    printf '\n  relais installer\n\n'

    t=$(target)
    v=$(resolve_version)
    name="relais-${v}-${t}"
    base="https://github.com/$REPO/releases/download/v${v}"

    say "version:  $v"
    say "platform: $t"
    say "into:     $BIN_DIR"
    printf '\n'

    tmp=$(mktemp -d)
    # Clean up on the way out however we leave, including Ctrl-C.
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "downloading…"
    fetch_to "$base/${name}.tar.gz" "$tmp/${name}.tar.gz" \
        || die "download failed: $base/${name}.tar.gz"

    # Checksums are not optional here. This binary launches model sessions
    # with your credentials inside your repositories; verifying what it is
    # before putting it in that position is the whole argument the project
    # makes about workers, applied to itself.
    #
    # So an UNVERIFIABLE download is fatal, not a warning: no SHA256SUMS,
    # no sha256 tool, no matching entry — each of them means nobody
    # checked what is about to run with your credentials, and the comment
    # above would otherwise be describing something the code does not do.
    # RELAIS_SKIP_CHECKSUM=1 is the explicit, typed-out way to say "I
    # accept an unverified binary"; there is no implicit one.
    skip_checksum="${RELAIS_SKIP_CHECKSUM:-}"
    if fetch_to "$base/SHA256SUMS" "$tmp/SHA256SUMS" 2> /dev/null; then
        if command -v sha256sum > /dev/null 2>&1; then
            got=$(sha256sum "$tmp/${name}.tar.gz" | cut -d' ' -f1)
        elif command -v shasum > /dev/null 2>&1; then
            got=$(shasum -a 256 "$tmp/${name}.tar.gz" | cut -d' ' -f1)
        else
            got=""
        fi
        if [ -n "$got" ]; then
            want=$(grep " ${name}.tar.gz\$" "$tmp/SHA256SUMS" | cut -d' ' -f1 | head -n 1)
            [ -n "$want" ] || die "SHA256SUMS has no entry for ${name}.tar.gz"
            [ "$got" = "$want" ] || die "checksum mismatch — refusing to install
    expected $want
    got      $got"
            ok "checksum verified"
        elif [ "$skip_checksum" = "1" ]; then
            warn "no sha256 tool found — installing UNVERIFIED because RELAIS_SKIP_CHECKSUM=1"
        else
            die "no sha256 tool found (install coreutils or perl) — refusing to install an
    unverified binary. Set RELAIS_SKIP_CHECKSUM=1 to accept that risk deliberately."
        fi
    elif [ "$skip_checksum" = "1" ]; then
        warn "no SHA256SUMS for this release — installing UNVERIFIED because RELAIS_SKIP_CHECKSUM=1"
    else
        die "no SHA256SUMS published for this release — refusing to install an unverified
    binary. Set RELAIS_SKIP_CHECKSUM=1 to accept that risk deliberately."
    fi

    tar xzf "$tmp/${name}.tar.gz" -C "$tmp"
    # Whether this is an upgrade decides what to say at the end: the
    # first-install steps are wrong advice the second time.
    upgrading=0
    [ -x "$BIN_DIR/relais" ] && upgrading=1
    mkdir -p "$BIN_DIR"
    # Write to a temporary name and rename over the destination: replacing a
    # RUNNING binary in place fails on some platforms, and rename is atomic, so
    # a half-copied relais never exists.
    [ -f "$tmp/$name/relais" ] || die "$name archive holds no relais binary"
    cp "$tmp/$name/relais" "$BIN_DIR/.relais.new"
    chmod 755 "$BIN_DIR/.relais.new"
    mv "$BIN_DIR/.relais.new" "$BIN_DIR/relais"
    ok "installed $BIN_DIR/relais"

    printf '\n'
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) warn "$BIN_DIR is not on your PATH — add it, or the /relais skill will report the binary as missing" ;;
    esac

    if [ "$upgrading" -eq 1 ]; then
        printf '  Upgraded. Runs already in the ledger stay readable; a trust grant in\n'
        printf '  machine.toml is bound to the policy, not to the binary, so it holds.\n\n'
        return
    fi
    printf '  Nothing runs yet, on purpose. In a repository:\n\n'
    printf '    relais doctor                      # what is installed and what is missing\n'
    printf '    relais init                        # write relais.toml, then commit it\n'
    printf '    relais plan --task task.json       # the route, and the trust grant to paste\n\n'
    printf '  The trust grant and the worker permission allowlist live in\n'
    printf '  ~/.config/relais/machine.toml — see the README.\n\n'
}

main "$@"
