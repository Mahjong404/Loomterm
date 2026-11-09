#Requires -Version 7.0
param(
    [switch]$Check,
    [switch]$Test,
    [string]$Filter,
    [string]$NativeManifest,
    [switch]$Offline,
    [string]$Target = 'x86_64-pc-windows-msvc'
)
$ErrorActionPreference = 'Stop'
if ($Test -and !$Filter) { throw 'Test requires an explicit adopted-feature filter; native TUI uses tools/test-cli.ps1.' }
if ($Check -and $Test) { throw 'Choose Check or Test.' }
$app = Join-Path (Split-Path $PSScriptRoot -Parent) 'apps/loomterm-cli'
$previousManifest = $env:LOOMTERM_NATIVE_MANIFEST
try {
    if ($NativeManifest) { $env:LOOMTERM_NATIVE_MANIFEST = (Resolve-Path -LiteralPath $NativeManifest).Path }
    # Cargo must discover this workspace's target-specific CRT configuration.
    Push-Location $app
    try {
        $command = if ($Test) { 'test' } elseif ($Check) { 'check' } else { 'build' }
        $arguments = @($command, '--locked', '--target', $Target)
        if ($Offline) { $arguments += '--offline' }
        if ($Test) { $arguments += @('--bin', 'loomterm', $Filter, '--', '--test-threads=1') }
        elseif ($Check) { $arguments += @('--workspace', '--all-targets') }
        & cargo @arguments
        if ($LASTEXITCODE -ne 0) { throw "CLI $command failed (exit $LASTEXITCODE)" }
    } finally { Pop-Location }
} finally { $env:LOOMTERM_NATIVE_MANIFEST = $previousManifest }
