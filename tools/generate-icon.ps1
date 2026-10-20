# Regenerate app-icon.ico and app-icon.png without downloading an SVG renderer.
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$root = Split-Path -Parent $PSScriptRoot
$env:CARGO_HTTP_CHECK_REVOKE = "false"
Push-Location $root
try {
    & .\cargo-worktree.ps1 build --example render_icon --locked --offline
    if ($LASTEXITCODE -ne 0) { throw "Icon renderer build failed." }
    $commonDir = & git rev-parse --path-format=absolute --git-common-dir
    if ($LASTEXITCODE -ne 0) { throw "Could not resolve repository." }
    $targetDir = if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
        Join-Path (Split-Path -Parent $commonDir) "target"
    } else {
        $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($env:CARGO_TARGET_DIR)
    }
    & (Join-Path $targetDir "debug/examples/render_icon.exe") $root
    if ($LASTEXITCODE -ne 0) { throw "Icon renderer failed." }
} finally {
    Pop-Location
}
