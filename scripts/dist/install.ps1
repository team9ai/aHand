# One-line installer for aHand (Windows).
# Usage (PowerShell):
#   iex (irm https://raw.githubusercontent.com/team9ai/aHand/main/scripts/dist/install.ps1)
#
# Or with parameters, download first then run:
#   $script = irm https://raw.githubusercontent.com/team9ai/aHand/main/scripts/dist/install.ps1
#   iex "& { $script } -Version 1.2.3"
#
# Environment variables (honoured when using iex form):
#   AHAND_VERSION -- install a specific rust version (e.g. "1.2.3")
#   AHAND_DIR     -- install directory (default: %USERPROFILE%\.ahand)
#
# Requires: PowerShell 5.1 or 7+, Windows 10 1803+ (for in-box tar/bsdtar).

param(
    [string]$ApiBase      = "https://api.github.com",
    [string]$DownloadBase = "https://github.com/team9ai/aHand/releases/download",
    [string]$InstallDir   = $(if ($env:AHAND_DIR) { $env:AHAND_DIR } else { Join-Path $env:USERPROFILE ".ahand" }),
    [string]$Version      = $env:AHAND_VERSION,
    [switch]$NoPathUpdate
)

$ErrorActionPreference = 'Stop'

# -- Helper: progress line ------------------------------------------------------

function Write-Step {
    param([string]$Message)
    Write-Host ""
    Write-Host "==> $Message" -ForegroundColor Cyan
}

# -- Detect architecture --------------------------------------------------------

function Get-Suffix {
    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($arch -eq 'AMD64') {
        return 'windows-x64'
    }
    if ($arch -eq 'ARM64') {
        throw "ERROR: No windows-arm64 artifacts published yet. ARM64 support is planned for a future release."
    }
    throw "ERROR: Unsupported architecture: $arch. Only AMD64 is currently supported on Windows."
}

# -- Resolve latest release versions -------------------------------------------

function Resolve-Versions {
    param([string]$PinnedVersion)

    $rustVer  = $null
    $adminVer = $null

    if ($PinnedVersion) {
        $rustVer  = $PinnedVersion
        $adminVer = $PinnedVersion
        Write-Host "  Using pinned version: $PinnedVersion"
        $versions = [PSCustomObject]@{ Rust = $rustVer; Admin = $adminVer }
        return $versions
    }

    Write-Host "  Fetching latest releases..."
    $releases = Invoke-RestMethod "$ApiBase/repos/team9ai/aHand/releases"

    foreach ($rel in $releases) {
        $tag = $rel.tag_name
        if ((-not $rustVer) -and ($tag -match '^rust-v([0-9A-Za-z.\-]+)$')) {
            $rustVer = $Matches[1]
        }
        if ((-not $adminVer) -and ($tag -match '^admin-v([0-9A-Za-z.\-]+)$')) {
            $adminVer = $Matches[1]
        }
        if ($rustVer -and $adminVer) {
            break
        }
    }

    if (-not $rustVer) {
        throw "Could not determine Rust release version. No release with a 'rust-v*' tag was found."
    }

    $adminLabel = if ($adminVer) { $adminVer } else { 'none' }
    Write-Host "  Versions: rust=$rustVer admin=$adminLabel"

    $versions = [PSCustomObject]@{ Rust = $rustVer; Admin = $adminVer }
    return $versions
}

# -- Download a file, with optional tolerance for failures ---------------------

function Invoke-Download {
    param(
        [string]$Url,
        [string]$Dest,
        [switch]$Tolerant
    )
    $filename = Split-Path $Dest -Leaf
    Write-Host "  Downloading $filename..."
    try {
        Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing
    } catch {
        if ($Tolerant) {
            Write-Host "  (optional) $filename not available -- skipping: $_" -ForegroundColor Yellow
            return $false
        }
        throw
    }
    return $true
}

# -- Verify SHA-256 checksums ---------------------------------------------------
# Checksum file format (sha256sum / shasum -a 256):
#   <hex>  <filename>

function Confirm-Checksums {
    param(
        [string]$ChecksumFile,
        [string[]]$FilesToVerify
    )
    $lines = Get-Content $ChecksumFile
    foreach ($filePath in $FilesToVerify) {
        $name = Split-Path $filePath -Leaf
        $expected = $null
        foreach ($line in $lines) {
            # Match "hex  filename" or "hex *filename"
            if ($line -match '^([0-9a-fA-F]{64})\s+[\*]?' + [regex]::Escape($name) + '$') {
                $expected = $Matches[1]
                break
            }
        }
        if (-not $expected) {
            Write-Host "  (checksum) No entry for $name in checksum file -- skipping verification for this file." -ForegroundColor Yellow
            continue
        }
        $actual = (Get-FileHash -Path $filePath -Algorithm SHA256).Hash
        if ($actual.ToUpper() -ne $expected.ToUpper()) {
            throw "Checksum mismatch for $name!`n  Expected: $($expected.ToUpper())`n  Actual  : $($actual.ToUpper())"
        }
        Write-Host "  Checksum OK: $name"
    }
}

# -- Install admin SPA via tar -------------------------------------------------
# Safe staged extraction:
#   1. Extract into a fresh temp staging dir.
#   2. Validate every item: reject reparse points and paths that escape staging root.
#   3. Only after validation succeeds: clear AdminDistDir and move contents in.
# A failed or malicious archive therefore leaves the existing AdminDistDir untouched.

function Install-AdminSpa {
    param(
        [string]$TarPath,
        [string]$AdminDistDir,
        [string]$ChecksumFile   # optional -- path to checksums-admin.txt
    )
    $tarCmd = Get-Command tar -ErrorAction SilentlyContinue
    if (-not $tarCmd) {
        Write-Host "  WARNING: 'tar' not found on PATH. Admin SPA will not be extracted." -ForegroundColor Yellow
        Write-Host "  (tar ships with Windows 10 1803+; install Git for Windows or upgrade to get it.)" -ForegroundColor Yellow
        return
    }

    # --- Optional checksum verification for admin-spa.tar.gz ---
    if ($ChecksumFile -and (Test-Path $ChecksumFile)) {
        Write-Host "  Verifying admin-spa.tar.gz checksum..."
        Confirm-Checksums -ChecksumFile $ChecksumFile -FilesToVerify @($TarPath)
    }

    # --- Extract into an isolated staging subdirectory ---
    $stagingExtract = Join-Path ([System.IO.Path]::GetTempPath()) "ahand-spa-stage-$([System.IO.Path]::GetRandomFileName())"
    New-Item -ItemType Directory -Path $stagingExtract -Force | Out-Null

    try {
        & tar -xzf $TarPath -C $stagingExtract
        if ($LASTEXITCODE -ne 0) {
            throw "tar extraction failed with exit code $LASTEXITCODE."
        }

        # --- Validate: no reparse points, no path escapes ---
        # Resolve staging root once (with trailing separator for prefix check)
        $stagingResolved = [System.IO.Path]::GetFullPath($stagingExtract)
        if (-not $stagingResolved.EndsWith([System.IO.Path]::DirectorySeparatorChar)) {
            $stagingResolved = $stagingResolved + [System.IO.Path]::DirectorySeparatorChar
        }

        $allItems = Get-ChildItem -LiteralPath $stagingExtract -Recurse -Force -ErrorAction SilentlyContinue
        foreach ($item in $allItems) {
            # Check for reparse points (symlinks, junctions, mount points)
            $attrs = (Get-Item -LiteralPath $item.FullName -Force).Attributes
            if ($attrs -band [System.IO.FileAttributes]::ReparsePoint) {
                throw "Archive contains a reparse point (symlink/junction) which is not permitted: $($item.FullName)"
            }

            # Check for path traversal escape
            $resolved = [System.IO.Path]::GetFullPath($item.FullName)
            if (-not $resolved.StartsWith($stagingResolved, [System.StringComparison]::OrdinalIgnoreCase)) {
                throw "Archive item escapes the staging directory (path traversal detected): $($item.FullName)"
            }
        }

        # --- Validation passed: swap staging into AdminDistDir ---
        if (Test-Path $AdminDistDir) {
            Remove-Item $AdminDistDir -Recurse -Force
        }
        $adminDistParent = Split-Path $AdminDistDir -Parent
        if (-not (Test-Path $adminDistParent)) {
            New-Item -ItemType Directory -Path $adminDistParent -Force | Out-Null
        }
        Move-Item -LiteralPath $stagingExtract -Destination $AdminDistDir -Force
        # Move-Item on PS 5.1 may require the destination not to already exist;
        # we removed it above so this should always succeed.

    } catch {
        # Clean up staging dir on failure; leave AdminDistDir untouched.
        if (Test-Path $stagingExtract) {
            Remove-Item $stagingExtract -Recurse -Force -ErrorAction SilentlyContinue
        }
        throw
    }
    # staging dir is now AdminDistDir -- no separate cleanup needed on success
}

# -- PATH management -----------------------------------------------------------

function Update-UserPath {
    param([string]$AddDir)
    $currentUser = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $currentUser) { $currentUser = '' }

    # Normalize: trim trailing backslashes, lowercase for comparison
    $normalizedAdd = $AddDir.TrimEnd('\').ToLower()
    $parts = $currentUser -split ';' | Where-Object { $_ -ne '' }
    $alreadyPresent = $false
    foreach ($p in $parts) {
        if ($p.TrimEnd('\').ToLower() -eq $normalizedAdd) {
            $alreadyPresent = $true
            break
        }
    }

    if ($alreadyPresent) {
        Write-Host "  $AddDir is already in your PATH."
        return
    }

    $newPath = ($parts + $AddDir) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host "  Added $AddDir to your user PATH."

    # Also update the current session
    $sessionNormalized = $env:Path -split ';' | ForEach-Object { $_.TrimEnd('\').ToLower() }
    if ($sessionNormalized -notcontains $normalizedAdd) {
        $env:Path = $env:Path.TrimEnd(';') + ';' + $AddDir
    }

    Write-Host ""
    Write-Host "  NOTE: Open a new terminal (or restart your shell) for the PATH change to take effect." -ForegroundColor Yellow
}

# -- Main ----------------------------------------------------------------------

$stagingDir = $null

try {
    Write-Step "Installing aHand..."

    # --- Architecture check ---
    $suffix = Get-Suffix
    Write-Host "  Architecture: $($env:PROCESSOR_ARCHITECTURE) => $suffix"

    # --- Version resolution ---
    $vers = Resolve-Versions -PinnedVersion $Version
    $rustVer  = $vers.Rust
    $adminVer = $vers.Admin

    # --- Staging directory ---
    $stagingDir = Join-Path $env:TEMP "ahand-install-$([System.IO.Path]::GetRandomFileName())"
    New-Item -ItemType Directory -Path $stagingDir -Force | Out-Null

    # --- Download Rust binaries (required) ---
    Write-Step "Downloading binaries (rust-v$rustVer)..."
    $rustBase  = "$DownloadBase/rust-v$rustVer"
    $daemonSrc = Join-Path $stagingDir "ahandd-$suffix.exe"
    $ctlSrc    = Join-Path $stagingDir "ahandctl-$suffix.exe"

    Invoke-Download -Url "$rustBase/ahandd-$suffix.exe"    -Dest $daemonSrc
    Invoke-Download -Url "$rustBase/ahandctl-$suffix.exe"  -Dest $ctlSrc

    # --- Download checksum file (optional) ---
    $checksumFile = Join-Path $stagingDir "checksums-rust.txt"
    $gotChecksums = Invoke-Download -Url "$rustBase/checksums-rust.txt" -Dest $checksumFile -Tolerant

    # Per-matrix checksum file is also acceptable if aggregated one is absent
    if (-not $gotChecksums) {
        $matrixChecksumFile = Join-Path $stagingDir "checksums-rust-$suffix.txt"
        $gotChecksums = Invoke-Download -Url "$rustBase/checksums-rust-$suffix.txt" -Dest $matrixChecksumFile -Tolerant
        if ($gotChecksums) {
            $checksumFile = $matrixChecksumFile
        }
    }

    # --- Verify checksums before installing anything ---
    if ($gotChecksums) {
        Write-Step "Verifying checksums..."
        Confirm-Checksums -ChecksumFile $checksumFile -FilesToVerify @($daemonSrc, $ctlSrc)
    } else {
        Write-Host "  (checksum file not available -- skipping integrity check)" -ForegroundColor Yellow
    }

    # --- Download admin SPA (optional) ---
    $adminTar          = $null
    $adminChecksumFile = $null
    if ($adminVer) {
        Write-Step "Downloading admin panel (admin-v$adminVer)..."
        $adminBase = "$DownloadBase/admin-v$adminVer"
        $adminTar  = Join-Path $stagingDir "admin-spa.tar.gz"
        $gotAdmin  = Invoke-Download -Url "$adminBase/admin-spa.tar.gz" -Dest $adminTar -Tolerant
        if (-not $gotAdmin) {
            $adminTar = $null
        } else {
            # Download admin checksum file (checksums-admin.txt) -- optional
            $adminChecksumDest = Join-Path $stagingDir "checksums-admin.txt"
            $gotAdminChecksum  = Invoke-Download -Url "$adminBase/checksums-admin.txt" -Dest $adminChecksumDest -Tolerant
            if ($gotAdminChecksum) {
                $adminChecksumFile = $adminChecksumDest
            }
        }
    }

    # NOTE: Browser/setup-browser.sh support is intentionally omitted on Windows in M2.
    # The browser setup script is a bash script targeting Linux/macOS environments.
    # Windows browser automation support is planned for M3.

    # --- Install binaries ---
    Write-Step "Installing to $InstallDir..."
    $binDir = Join-Path $InstallDir "bin"
    New-Item -ItemType Directory -Path $binDir -Force | Out-Null

    Copy-Item $daemonSrc (Join-Path $binDir "ahandd.exe")   -Force
    Write-Host "  Installed ahandd.exe"
    Copy-Item $ctlSrc (Join-Path $binDir "ahandctl.exe") -Force
    Write-Host "  Installed ahandctl.exe"

    # --- Install admin SPA ---
    if ($adminTar) {
        Write-Step "Extracting admin panel..."
        $adminDistDir = Join-Path $InstallDir "admin\dist"
        Install-AdminSpa -TarPath $adminTar -AdminDistDir $adminDistDir -ChecksumFile $adminChecksumFile
        Write-Host "  Admin panel extracted to $adminDistDir"
    }

    # --- Write version marker ---
    $versionFile = Join-Path $InstallDir "version"
    [System.IO.File]::WriteAllText($versionFile, "$rustVer`n", [System.Text.UTF8Encoding]::new($false))
    Write-Host "  Version file written: $rustVer"

    # --- Update PATH ---
    if (-not $NoPathUpdate) {
        Write-Step "Updating PATH..."
        Update-UserPath -AddDir $binDir
    }

    # --- Success summary ---
    Write-Step "aHand installed successfully!"
    Write-Host ""
    Write-Host "  Installed to : $InstallDir"
    Write-Host "  ahandd       : $(Join-Path $binDir 'ahandd.exe')"
    Write-Host "  ahandctl     : $(Join-Path $binDir 'ahandctl.exe')"
    Write-Host ""
    Write-Host "Get started:"
    Write-Host ""
    Write-Host "  1. Configure your instance:"
    Write-Host ""
    Write-Host "       ahandctl configure"
    Write-Host ""
    Write-Host "  2. Start the daemon:"
    Write-Host ""
    Write-Host "       ahandctl start"
    Write-Host ""
    Write-Host "  3. See all commands:"
    Write-Host ""
    Write-Host "       ahandctl --help"
    Write-Host ""

} finally {
    if ($stagingDir -and (Test-Path $stagingDir)) {
        Remove-Item $stagingDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
