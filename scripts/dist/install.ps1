#Requires -Version 5.1
<#
.SYNOPSIS
    Install aHand (ahandd + ahandctl) on Windows.
.DESCRIPTION
    Downloads prebuilt binaries from GitHub Releases and installs to ~/.ahand/bin/.
    Verifies SHA-256 checksums and adds the install directory to the user PATH.
.EXAMPLE
    irm https://raw.githubusercontent.com/team9ai/aHand/main/scripts/dist/install.ps1 | iex
.PARAMETER Version
    Specific version to install (default: latest).
.PARAMETER InstallDir
    Directory to install binaries (default: $env:USERPROFILE\.ahand\bin).
#>
param(
    [string]$Version = "",
    [string]$InstallDir = "$env:USERPROFILE\.ahand\bin"
)

$ErrorActionPreference = "Stop"
$REPO = "team9ai/aHand"

# ── Detect architecture ──────────────────────────────────────────────

$arch = if ([Environment]::Is64BitOperatingSystem) {
    if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" }
} else {
    Write-Error "32-bit Windows is not supported."
    exit 1
}
$suffix = "windows-$arch"

# ── Determine version ────────────────────────────────────────────────

if (-not $Version) {
    Write-Host "Fetching latest version..."
    try {
        $release = Invoke-RestMethod "https://api.github.com/repos/$REPO/releases/latest" `
            -Headers @{ "User-Agent" = "ahand-installer" }
        $Version = $release.tag_name -replace '^rust-v', ''
    } catch {
        Write-Error "Failed to fetch latest release: $_"
        exit 1
    }
}

if (-not $Version) {
    Write-Error "Could not determine version to install."
    exit 1
}

Write-Host ""
Write-Host "Installing aHand v$Version ($suffix)..."

# ── Create install directory ─────────────────────────────────────────

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

# ── Download binaries ────────────────────────────────────────────────

$baseUrl = "https://github.com/$REPO/releases/download/rust-v$Version"

foreach ($binary in @("ahandd", "ahandctl")) {
    $asset = "$binary-$suffix.exe"
    $url = "$baseUrl/$asset"
    $dest = Join-Path $InstallDir "$binary.exe"

    Write-Host "  Downloading $asset..."
    try {
        Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing
    } catch {
        Write-Error "Failed to download $asset from $url : $_"
        exit 1
    }
    Write-Host "  Installed: $dest"
}

# ── Verify checksums ────────────────────────────────────────────────

# Build a lookup from asset names (in checksum file) to installed paths.
$nameToPath = @{
    "ahandd-$suffix.exe"   = Join-Path $InstallDir "ahandd.exe"
    "ahandctl-$suffix.exe" = Join-Path $InstallDir "ahandctl.exe"
}

$checksumUrl = "$baseUrl/checksums-rust-$suffix.txt"
try {
    $checksums = Invoke-RestMethod -Uri $checksumUrl -Headers @{ "User-Agent" = "ahand-installer" }
    foreach ($line in $checksums -split "`n") {
        $line = $line.Trim()
        if ($line -match "^([0-9a-f]+)\s+(.+)$") {
            $expected = $Matches[1]
            $assetName = $Matches[2].Trim()
            $filePath = $nameToPath[$assetName]
            if ($filePath -and (Test-Path $filePath)) {
                $actual = (Get-FileHash $filePath -Algorithm SHA256).Hash.ToLower()
                if ($actual -ne $expected) {
                    Remove-Item $filePath -Force -ErrorAction SilentlyContinue
                    Write-Error "Checksum mismatch for $assetName! Expected $expected, got $actual. File removed."
                    exit 1
                } else {
                    Write-Host "  Checksum OK: $assetName"
                }
            }
        }
    }
} catch {
    Write-Error "Could not verify checksums: $_"
    Write-Error "Installation aborted — cannot verify binary integrity."
    exit 1
}

# ── Write version marker ────────────────────────────────────────────

$versionFile = Join-Path (Split-Path $InstallDir -Parent) "version"
$Version | Out-File -FilePath $versionFile -Encoding utf8 -NoNewline

# ── Add to PATH ─────────────────────────────────────────────────────

$userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($userPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("PATH", "$userPath;$InstallDir", "User")
    Write-Host ""
    Write-Host "Added $InstallDir to user PATH."
    Write-Host "Restart your terminal for PATH changes to take effect."
}

# ── Done ─────────────────────────────────────────────────────────────

Write-Host ""
Write-Host "aHand v$Version installed successfully!"
Write-Host "  ahandd:   $(Join-Path $InstallDir 'ahandd.exe')"
Write-Host "  ahandctl: $(Join-Path $InstallDir 'ahandctl.exe')"
Write-Host ""
Write-Host "Get started:"
Write-Host "  ahandctl configure"
Write-Host ""
