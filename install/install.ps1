<#
.SYNOPSIS
    Install the relais binary on Windows. Nothing else.

.DESCRIPTION
    irm https://raw.githubusercontent.com/fredericrous/relais/main/install/install.ps1 | iex

    The counterpart to install/install.sh, which is POSIX sh and therefore
    reaches Windows only through Git Bash. This one runs in the PowerShell
    that ships with Windows.

    Like its sibling it deliberately turns NOTHING on. It downloads a verified
    binary, puts it on your PATH, and says what to run next. It writes no
    policy and grants no trust.

.PARAMETER Version
    A specific release, e.g. "v1.0.0". Defaults to the latest.

.PARAMETER BinDir
    Where to put the executable. Defaults to $HOME\.local\bin, the
    conventional per-user location.
#>
[CmdletBinding()]
param(
    [string]$Version = $env:RELAIS_VERSION,
    [string]$BinDir  = $env:RELAIS_BIN_DIR
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Repo = 'fredericrous/relais'
if (-not $BinDir)  { $BinDir  = Join-Path $HOME '.local\bin' }
if (-not $Version) { $Version = 'latest' }

function Write-Ok   ($m) { Write-Host "  [ok] $m"   -ForegroundColor Green }
function Write-Warn ($m) { Write-Host "  [!]  $m"   -ForegroundColor Yellow }
function Fail       ($m) { Write-Host "  [x]  $m"   -ForegroundColor Red; exit 1 }

# Windows PowerShell 5.1 still negotiates TLS 1.0 by default, which GitHub
# refuses outright — the download fails with a connection error that says
# nothing about the cause. PowerShell 7 already defaults to TLS 1.2+.
try {
    [Net.ServicePointManager]::SecurityProtocol =
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # Not fatal: PS7 has no mutable ServicePointManager and does not need one.
}

Write-Host ''
Write-Host '  relais installer'
Write-Host ''

# Only x86_64 is published today. Windows on ARM runs x64 binaries through
# emulation, so this is a working install rather than a refusal — said out
# loud, because a silently emulated binary is a surprise worth naming.
$arch = $env:PROCESSOR_ARCHITECTURE
$target = 'x86_64-pc-windows-msvc'
if ($arch -eq 'ARM64') {
    Write-Warn 'ARM64 detected — installing the x64 build, which Windows runs under emulation.'
}

if ($Version -eq 'latest') {
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" `
            -Headers @{ 'User-Agent' = 'relais-installer' }
        $Version = $release.tag_name
    } catch {
        Fail "could not determine the latest release (rate limited? pass -Version v1.2.3): $_"
    }
}
$v = $Version -replace '^v', ''

$name = "relais-$v-$target"
$base = "https://github.com/$Repo/releases/download/v$v"

Write-Host "  version:  $v"
Write-Host "  platform: $target"
Write-Host "  into:     $BinDir"
Write-Host ''

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("relais-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null

try {
    $zip = Join-Path $tmp "$name.zip"
    Write-Host '  downloading...'
    try {
        Invoke-WebRequest -Uri "$base/$name.zip" -OutFile $zip -UseBasicParsing
    } catch {
        Fail "download failed: $base/$name.zip"
    }

    # Checksums are not optional here. This binary launches model sessions
    # with your credentials inside your repositories; verifying what it is
    # before putting it in that position is the argument the project makes
    # about workers, applied to itself.
    #
    # So an UNVERIFIABLE download is fatal, not a warning, exactly as in
    # install.sh: RELAIS_SKIP_CHECKSUM=1 is the explicit way to accept an
    # unverified binary, and there is no implicit one.
    $skipChecksum = ($env:RELAIS_SKIP_CHECKSUM -eq '1')
    $sums = Join-Path $tmp 'SHA256SUMS'
    try {
        Invoke-WebRequest -Uri "$base/SHA256SUMS" -OutFile $sums -UseBasicParsing
    } catch {
        $sums = $null
        if ($skipChecksum) {
            Write-Warn 'no SHA256SUMS for this release - installing UNVERIFIED because RELAIS_SKIP_CHECKSUM=1'
        } else {
            Fail "no SHA256SUMS published for this release - refusing to install an unverified binary.`n         Set RELAIS_SKIP_CHECKSUM=1 to accept that risk deliberately."
        }
    }

    if ($sums) {
        $got  = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLower()
        $line = Select-String -Path $sums -Pattern ([regex]::Escape("$name.zip")) |
                Select-Object -First 1
        if (-not $line) { Fail "SHA256SUMS has no entry for $name.zip" }
        $want = ($line.Line -split '\s+')[0].ToLower()
        if ($got -ne $want) {
            Fail "checksum mismatch - refusing to install`n    expected $want`n    got      $got"
        }
        Write-Ok 'checksum verified'
    }

    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $src = Join-Path $tmp $name
    # Whether this is an upgrade decides what to say at the end: the
    # first-install steps are wrong advice the second time.
    $upgrading = Test-Path (Join-Path $BinDir 'relais.exe')
    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null

    # An archive with no relais.exe in it is a failed install, not a quiet
    # one: the sh sibling dies on exactly this, and printing the "here is
    # what to run next" epilogue after installing nothing sent people
    # looking for a binary that was never written (C8).
    $installed = 0
    foreach ($exe in @('relais.exe')) {
        $from = Join-Path $src $exe
        if (Test-Path $from) {
            $to = Join-Path $BinDir $exe
            # Copy beside the destination and move it into place: replacing a
            # RUNNING executable in place fails on Windows with a sharing
            # violation, and a move is atomic, so a half-copied relais.exe
            # never exists.
            $staged = "$to.new"
            Copy-Item -Path $from -Destination $staged -Force
            Move-Item -Path $staged -Destination $to -Force
            Write-Ok "installed $to"
            $installed++
        }
    }
    if ($installed -eq 0) {
        Fail "$name archive holds no relais.exe - nothing was installed"
    }
} finally {
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ''

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$BinDir*") {
    Write-Warn "$BinDir is not on your PATH. To run it by name:"
    Write-Host ''
    Write-Host "         [Environment]::SetEnvironmentVariable('Path', `"`$env:Path;$BinDir`", 'User')"
    Write-Host ''
}

if ($upgrading) {
    Write-Host '  Upgraded. Runs already in the ledger stay readable; a trust grant in'
    Write-Host '  machine.toml is bound to the policy, not to the binary, so it holds.'
    Write-Host ''
} else {
    Write-Host '  Nothing runs yet, on purpose. In a repository:'
    Write-Host ''
    Write-Host '    relais doctor                      # what is installed and what is missing'
    Write-Host '    relais init                        # write relais.toml, then commit it'
    Write-Host '    relais plan --task task.json       # the route, and the trust grant to paste'
    Write-Host ''
    Write-Host '  The trust grant and the worker permission allowlist live in'
    Write-Host '  ~\.config\relais\machine.toml - see the README.'
    Write-Host ''
}
