[CmdletBinding()]
param(
    [Parameter()]
    [string[]] $Target = @("$env:SystemDrive\")
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repoRoot 'Cargo.toml'
$cargoArgs = @(
    'run'
    '--locked'
    '--manifest-path'
    $manifest
    '--package'
    'kranz'
    '--'
    'sandbox-prepare'
)

foreach ($requestedRoot in ($Target | Sort-Object -Unique)) {
    if ([string]::IsNullOrWhiteSpace($requestedRoot)) {
        throw 'AppContainer host preparation received an empty target.'
    }
    $root = [System.IO.Path]::GetFullPath($requestedRoot)
    if ($root -notmatch '^[A-Za-z]:\\$') {
        throw "AppContainer host preparation target must be a local drive root (X:\): $requestedRoot"
    }
    $cargoArgs += @('--target', $root)
}

# The Rust command owns the security-sensitive operation so the same exact
# drive-root DACL path and null-device descriptor path are compiled,
# cross-checked, and used by both operators and protected CI. This script is
# only the elevated source-checkout convenience wrapper; rerun it after boot
# because Windows resets \Device\Null's descriptor.
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) {
    throw "kranz sandbox-prepare failed with exit code $LASTEXITCODE"
}
