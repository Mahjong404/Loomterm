#Requires -Version 7.0
param([string]$HostSource, [string]$HostTarget, [switch]$LauncherOnly, [switch]$Controller)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$previousTarget = $env:CARGO_TARGET_DIR
try {
    if (!$LauncherOnly) {
        if (!$HostSource -or !$HostTarget) { throw 'Supply pinned, prepared HostSource and separate HostTarget paths.' }
        $HostSource = (Resolve-Path -LiteralPath $HostSource).Path
        $HostTarget = [IO.Path]::GetFullPath($HostTarget)
        & python (Join-Path $PSScriptRoot 'prepare-window-host.py') --verify $HostSource
        if ($LASTEXITCODE) { throw 'Host source does not match the prepared adapter' }
        Push-Location $HostSource
        try {
            # Upstream build.rs stages native assets at this fixed location.
            New-Item -ItemType Directory -Force target/debug | Out-Null
            $env:CARGO_TARGET_DIR = $HostTarget
            & cargo build --locked --target x86_64-pc-windows-msvc -p wezterm-gui
            if ($LASTEXITCODE) { throw 'Window host build failed' }
            if ($Controller) {
                & cargo rustc --locked --target x86_64-pc-windows-msvc -p wezterm -- -C link-arg=/STACK:8388608
                if ($LASTEXITCODE) { throw 'Window test controller build failed' }
            }
            $output = Join-Path $HostTarget 'x86_64-pc-windows-msvc/debug'
            foreach ($asset in @('conhost', 'angle')) {
                Get-ChildItem -LiteralPath "assets/windows/$asset" -File | Copy-Item -Destination $output -Force
            }
            Copy-Item -LiteralPath assets/windows/mesa -Destination $output -Recurse -Force
            $hostExe = Join-Path $output 'wezterm-gui.exe'
            (Get-FileHash -Algorithm SHA256 -LiteralPath $hostExe).Hash.ToLowerInvariant() |
                Set-Content -LiteralPath (Join-Path $output 'loomterm-host.sha256')
        } finally { Pop-Location }
    }
    Push-Location (Join-Path $repo 'apps/loomterm-window')
    try {
        $env:CARGO_TARGET_DIR = Join-Path $repo 'target/loomterm-window'
        & cargo build --locked --offline --target x86_64-pc-windows-msvc
        if ($LASTEXITCODE) { throw 'Window launcher build failed' }
    } finally { Pop-Location }
} finally { $env:CARGO_TARGET_DIR = $previousTarget }
