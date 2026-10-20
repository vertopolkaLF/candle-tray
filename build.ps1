# Builds multi-arch portable zips and NSIS installers into dist/<version>/.
# Usage:
#   .\build.ps1
#   .\build.ps1 -Arch x64,arm64
#   .\build.ps1 -SkipInstaller

[CmdletBinding()]
param(
    [string]$OutDir = "dist",
    [ValidateSet("x64", "arm64")]
    [string[]]$Arch = @("x64", "arm64"),
    [switch]$SkipInstaller
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$env:CARGO_HTTP_CHECK_REVOKE = "false"

$Root = $PSScriptRoot
$ProductName = "Candle Tray"
$Publisher = "Candle Tray"
$AppExeName = "candle-tray.exe"
# Mirrored by Tauri; SourceForge HTML landing pages are not ZIP archives.
$NsisVersion = "3.11"
$NsisUrl = "https://github.com/tauri-apps/binary-releases/releases/download/nsis-$NsisVersion/nsis-$NsisVersion.zip"

# The app embeds its assets; ship only the executable and license notices.
$RuntimeFiles = @(
    "candle-tray.exe"
)

$TargetMap = [ordered]@{
    x64   = "x86_64-pc-windows-msvc"
    arm64 = "aarch64-pc-windows-msvc"
}

function Get-CargoVersion {
    $cargoToml = Join-Path $Root "Cargo.toml"
    $match = Select-String -LiteralPath $cargoToml -Pattern '^\s*version\s*=\s*"([^"]+)"' |
        Select-Object -First 1
    if (-not $match) {
        throw "Could not read package version from Cargo.toml"
    }
    return $match.Matches[0].Groups[1].Value
}

$RequiredRuntimeFiles = @(
    "candle-tray.exe"
)

function Copy-RuntimeItem {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination,
        [switch]$Optional
    )

    if (-not (Test-Path -LiteralPath $Source)) {
        if ($Optional) {
            Write-Warning "Skipping missing optional runtime item: $Source"
            return
        }
        throw "Missing required runtime item: $Source"
    }

    $parent = Split-Path -Parent $Destination
    if ($parent -and -not (Test-Path -LiteralPath $parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }

    Copy-Item -LiteralPath $Source -Destination $Destination -Recurse -Force
}

function Ensure-RustTarget {
    param([Parameter(Mandatory = $true)][string]$Triple)

    $installed = & rustup target list --installed
    if ($LASTEXITCODE -ne 0) {
        throw "rustup target list --installed failed with exit code $LASTEXITCODE"
    }
    if ($installed -notcontains $Triple) {
        Write-Host "==> rustup target add $Triple"
        & rustup target add $Triple
        if ($LASTEXITCODE -ne 0) {
            throw "rustup target add $Triple failed with exit code $LASTEXITCODE"
        }
    }
}

function Get-VcVarsAllPath {
    $vswhereCandidates = @(
        (Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"),
        (Join-Path $env:ProgramFiles "Microsoft Visual Studio\Installer\vswhere.exe")
    ) | Where-Object { Test-Path -LiteralPath $_ }

    foreach ($vswhere in $vswhereCandidates) {
        $installPath = & $vswhere -latest -products * -property installationPath
        if ($LASTEXITCODE -eq 0 -and $installPath) {
            $vcvarsall = Join-Path $installPath.Trim() "VC\Auxiliary\Build\vcvarsall.bat"
            if (Test-Path -LiteralPath $vcvarsall) {
                # Some VS environment scripts invoke vswhere by name internally.
                $env:PATH = "$(Split-Path -Parent $vswhere);$env:PATH"
                return $vcvarsall
            }
        }
    }

    throw "Visual Studio C++ Build Tools were not found. Install the Desktop development with C++ workload, including the ARM64 build tools."
}

function Import-VisualStudioBuildEnvironment {
    param([Parameter(Mandatory = $true)][string]$Architecture)

    $vcvarsArgument = switch ($Architecture) {
        "x64" { "x64" }
        "arm64" { "x64_arm64" }
        default { throw "Unknown architecture: $Architecture" }
    }

    $vcvarsall = Get-VcVarsAllPath
    $command = 'call "{0}" {1} >nul && set' -f $vcvarsall, $vcvarsArgument
    $environment = & cmd.exe /d /s /c $command
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to initialize the Visual Studio $Architecture build environment. Ensure its C++ tools are installed."
    }

    foreach ($line in $environment) {
        if ($line -match '^([^=]+)=(.*)$') {
            [Environment]::SetEnvironmentVariable($matches[1], $matches[2], "Process")
        }
    }

    $compiler = Get-Command cl.exe -ErrorAction SilentlyContinue
    if (-not $compiler) {
        throw "Visual Studio did not provide cl.exe for $Architecture. Install the corresponding C++ build tools."
    }

    if ($Architecture -ne "arm64") {
        return
    }

    # ring currently requires Clang (not MSVC's cl.exe) for Windows ARM64.
    # The VS LLVM component is not always added to PATH by vcvarsall, so look
    # in its standard location as well as a standalone LLVM installation.
    $clang = Get-Command clang.exe -ErrorAction SilentlyContinue
    if (-not $clang) {
        $clangCandidates = @(@(
            (Join-Path $env:VSINSTALLDIR "VC\Tools\Llvm\x64\bin\clang.exe"),
            (Join-Path $env:ProgramFiles "LLVM\bin\clang.exe")
        ) | Where-Object { Test-Path -LiteralPath $_ })

        if ($clangCandidates) {
            $clangDirectory = Split-Path -Parent $clangCandidates[0]
            $env:PATH = "$clangDirectory;$env:PATH"
            $clang = Get-Command clang.exe -ErrorAction SilentlyContinue
        }
    }

    if (-not $clang) {
        throw "ARM64 builds require clang because ring v0.17 does not support MSVC for Windows ARM64. Install the Visual Studio 'C++ Clang tools for Windows' component or LLVM, then run the build again."
    }
}

function New-PortablePackage {
    param(
        [Parameter(Mandatory = $true)][string]$ReleaseDir,
        [Parameter(Mandatory = $true)][string]$PackageDir
    )

    New-Item -ItemType Directory -Path $PackageDir -Force | Out-Null

    foreach ($name in $RuntimeFiles) {
        $optional = $name -notin $RequiredRuntimeFiles
        Copy-RuntimeItem `
            -Source (Join-Path $ReleaseDir $name) `
            -Destination (Join-Path $PackageDir $name) `
            -Optional:$optional
    }

    foreach ($notice in @("LICENSE", "THIRD_PARTY_NOTICES.md")) {
        Copy-RuntimeItem -Source (Join-Path $Root $notice) -Destination (Join-Path $PackageDir $notice)
    }
}

function New-ZipFromDirectory {
    param(
        [Parameter(Mandatory = $true)][string]$SourceDir,
        [Parameter(Mandatory = $true)][string]$ZipPath
    )

    if (Test-Path -LiteralPath $ZipPath) {
        Remove-Item -LiteralPath $ZipPath -Force
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $SourceDir,
        $ZipPath,
        [System.IO.Compression.CompressionLevel]::Optimal,
        $true
    )
}

function Find-MakeNsis {
    $cmd = Get-Command makensis -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }

    $candidates = @(
        (Join-Path $Root ".tools\nsis-$NsisVersion\makensis.exe"),
        "${env:ProgramFiles(x86)}\NSIS\makensis.exe",
        "${env:ProgramFiles}\NSIS\makensis.exe"
    )
    foreach ($path in $candidates) {
        if ($path -and (Test-Path -LiteralPath $path)) {
            return $path
        }
    }
    return $null
}

function Test-ZipFile {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        return $false
    }
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    return ($bytes.Length -ge 4 -and $bytes[0] -eq 0x50 -and $bytes[1] -eq 0x4B)
}

function Ensure-MakeNsis {
    $existing = Find-MakeNsis
    if ($existing) {
        return $existing
    }

    $toolsDir = Join-Path $Root ".tools"
    $zipPath = Join-Path $toolsDir "nsis-$NsisVersion.zip"
    $extractDir = Join-Path $toolsDir "nsis-$NsisVersion"

    New-Item -ItemType Directory -Path $toolsDir -Force | Out-Null
    if (-not (Test-ZipFile -Path $zipPath)) {
        Write-Host "==> downloading NSIS $NsisVersion"
        if (Test-Path -LiteralPath $zipPath) {
            Remove-Item -LiteralPath $zipPath -Force
        }
        & curl.exe --ssl-no-revoke -fsSL --retry 3 -o $zipPath $NsisUrl
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to download NSIS from $NsisUrl (exit $LASTEXITCODE)"
        }
        if (-not (Test-ZipFile -Path $zipPath)) {
            throw "Downloaded NSIS archive is not a valid zip: $zipPath"
        }
    }

    Expand-Archive -LiteralPath $zipPath -DestinationPath $toolsDir -Force

    $makensis = Join-Path $extractDir "makensis.exe"
    if (-not (Test-Path -LiteralPath $makensis)) {
        throw "NSIS extract succeeded but makensis.exe was not found at $makensis"
    }
    return $makensis
}

function New-NsisInstaller {
    param(
        [Parameter(Mandatory = $true)][string]$MakeNsis,
        [Parameter(Mandatory = $true)][string]$ArchName,
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)][string]$PackageDir,
        [Parameter(Mandatory = $true)][string]$OutFile,
        [Parameter(Mandatory = $true)][string]$WorkDir
    )

    $templatePath = Join-Path $Root "packaging\installer.nsi"
    if (-not (Test-Path -LiteralPath $templatePath)) {
        throw "Missing NSIS template: $templatePath"
    }

    $iconFile = Join-Path $Root "assets\app-icon.ico"
    # Per-user install without elevation.
    $installDir = "`$LOCALAPPDATA\Programs\$ProductName"

    $nsiPath = Join-Path $WorkDir "installer-$ArchName.nsi"
    $rendered = Get-Content -LiteralPath $templatePath -Raw -Encoding UTF8
    $replacements = [ordered]@{
        "{{PRODUCT_NAME}}"  = $ProductName
        "{{VERSION}}"       = $Version
        "{{PUBLISHER}}"     = $Publisher
        "{{ARCH}}"          = $ArchName
        "{{OUT_FILE}}"      = ($OutFile -replace '\\', '/')
        "{{SOURCE_DIR}}"    = ($PackageDir -replace '\\', '/')
        "{{ICON_FILE}}"     = ($iconFile -replace '\\', '/')
        "{{INSTALL_DIR}}"   = $installDir
    }
    foreach ($key in $replacements.Keys) {
        $rendered = $rendered.Replace($key, [string]$replacements[$key])
    }

    # NSIS expects ANSI/UTF-8 without BOM for reliable parsing of paths.
    $utf8NoBom = New-Object System.Text.UTF8Encoding $false
    [System.IO.File]::WriteAllText($nsiPath, $rendered, $utf8NoBom)

    Write-Host "==> makensis ($ArchName)"
    & $MakeNsis /V2 $nsiPath
    if ($LASTEXITCODE -ne 0) {
        throw "makensis failed for $ArchName with exit code $LASTEXITCODE"
    }
    if (-not (Test-Path -LiteralPath $OutFile)) {
        throw "Installer was not created: $OutFile"
    }
}

# --- main --------------------------------------------------------------------

$Version = Get-CargoVersion
$VersionDir = Join-Path $Root (Join-Path $OutDir $Version)

Write-Host "==> release $Version"
Write-Host "    arches : $($Arch -join ', ')"
Write-Host "    output : $VersionDir"

# Keep existing artifacts for other architectures. Each invocation owns only
# its unique staging directory and replaces its selected output files.
New-Item -ItemType Directory -Path $VersionDir -Force | Out-Null
$StageRoot = Join-Path $Root (Join-Path $OutDir (".staging/" + [guid]::NewGuid().ToString("N")))
$WorkDir = Join-Path $StageRoot "_work"
New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null
$commonDir = & git -C $Root rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0) { throw "Could not resolve the repository target directory." }
$targetDir = if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    Join-Path (Split-Path -Parent $commonDir) "target"
} else {
    $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($env:CARGO_TARGET_DIR)
}

$makeNsis = $null
if (-not $SkipInstaller) {
    $makeNsis = Ensure-MakeNsis
    Write-Host "    nsis   : $makeNsis"
}

$artifacts = @()

Push-Location $Root
try {
    foreach ($archName in $Arch) {
        $triple = $TargetMap[$archName]
        if (-not $triple) {
            throw "Unknown architecture: $archName"
        }

        Write-Host ""
        Write-Host "======== $archName ($triple) ========"
        Import-VisualStudioBuildEnvironment -Architecture $archName
        Ensure-RustTarget -Triple $triple

        Write-Host "==> cargo build --release --target $triple"
        & (Join-Path $Root "cargo-worktree.ps1") build --release --target $triple --locked
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed for $triple with exit code $LASTEXITCODE"
        }

        $releaseDir = Join-Path $targetDir "$triple\release"
        $packageName = "candle-tray-$Version-$archName"
        $packageDir = Join-Path $StageRoot $packageName

        Write-Host "==> packaging portable ($archName)"
        New-PortablePackage -ReleaseDir $releaseDir -PackageDir $packageDir

        if (-not (Test-Path -LiteralPath (Join-Path $packageDir $AppExeName))) {
            throw "Packaged binary missing: $(Join-Path $packageDir $AppExeName)"
        }

        $zipPath = Join-Path $VersionDir "$packageName-portable.zip"
        Write-Host "==> zip $([IO.Path]::GetFileName($zipPath))"
        New-ZipFromDirectory -SourceDir $packageDir -ZipPath $zipPath
        $artifacts += $zipPath

        if (-not $SkipInstaller) {
            $setupPath = Join-Path $VersionDir "$packageName-setup.exe"
            New-NsisInstaller `
                -MakeNsis $makeNsis `
                -ArchName $archName `
                -Version $Version `
                -PackageDir $packageDir `
                -OutFile $setupPath `
                -WorkDir $WorkDir
            $artifacts += $setupPath
        }
    }
}
finally {
    Pop-Location
}

Write-Host ""
Write-Host "Cleaning staging..."
$resolvedStage = [IO.Path]::GetFullPath($StageRoot)
$stageParent = [IO.Path]::GetFullPath((Join-Path $Root (Join-Path $OutDir ".staging"))).TrimEnd('\') + '\'
if (-not $resolvedStage.StartsWith($stageParent, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Refusing to remove staging outside $stageParent"
}
Remove-Item -LiteralPath $resolvedStage -Recurse -Force

Write-Host ""
Write-Host "Done."
Write-Host "  Version : $Version"
Write-Host "  Output  : $VersionDir"
foreach ($path in $artifacts) {
    $item = Get-Item -LiteralPath $path
    $sizeMb = [math]::Round($item.Length / 1MB, 2)
    Write-Host ("  - {0} ({1} MB)" -f $item.Name, $sizeMb)
}
