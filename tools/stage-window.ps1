#Requires -Version 7.0
param(
    [Parameter(Mandatory)][string]$HostSource,
    [Parameter(Mandatory)][string]$HostTarget,
    [Parameter(Mandatory)][string]$CliExe
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$source = (Resolve-Path -LiteralPath $HostSource).Path
$target = (Resolve-Path -LiteralPath $HostTarget).Path
& python (Join-Path $PSScriptRoot 'prepare-window-host.py') --verify $source
if ($LASTEXITCODE) { throw 'Unrecognized host adapter source' }
$hostDir = Join-Path $target 'x86_64-pc-windows-msvc/debug'
$exe = Join-Path $hostDir 'wezterm-gui.exe'
$hash = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
if ((Get-Content -LiteralPath (Join-Path $hostDir 'loomterm-host.sha256') -Raw).Trim() -ne $hash) {
    throw 'Host binary differs from build manifest'
}
$out = Join-Path $repo 'target/loomterm-window/dev'
New-Item -ItemType Directory -Force $out, (Join-Path $out 'host'), (Join-Path $out 'notices') | Out-Null
Copy-Item -LiteralPath (Join-Path $repo 'target/loomterm-window/x86_64-pc-windows-msvc/debug/loomterm-window.exe') -Destination $out -Force
Copy-Item -LiteralPath (Resolve-Path -LiteralPath $CliExe).Path -Destination (Join-Path $out 'loomterm.exe') -Force
foreach ($name in @('wezterm-gui.exe', 'loomterm-host.sha256', 'conpty.dll', 'OpenConsole.exe', 'libEGL.dll', 'libGLESv2.dll')) {
    Copy-Item -LiteralPath (Join-Path $hostDir $name) -Destination (Join-Path $out 'host') -Force
}
Copy-Item -LiteralPath (Join-Path $source 'assets/windows/mesa') -Destination (Join-Path $out 'host') -Recurse -Force
Copy-Item -LiteralPath (Join-Path $source 'LICENSE.md') -Destination (Join-Path $out 'notices/WezTerm-LICENSE.md') -Force
Copy-Item -LiteralPath (Join-Path $repo 'apps/loomterm-cli/LICENSE') -Destination (Join-Path $out 'notices/CLI-LICENSE') -Force
# Retain source/font/native notices alongside this local development artifact.
# This is not the complete transitive redistribution inventory required for release.
Get-ChildItem -LiteralPath (Join-Path $source 'assets'), (Join-Path $source 'deps') -Recurse -File |
    Where-Object { $_.Name -match '^(LICENSE|COPYING|NOTICE|OFL|COPYRIGHT)' } |
    ForEach-Object {
        $relative = [IO.Path]::GetRelativePath($source, $_.FullName)
        $dest = Join-Path $out "notices/$relative"
        New-Item -ItemType Directory -Force (Split-Path -Parent $dest) | Out-Null
        Copy-Item -LiteralPath $_.FullName -Destination $dest -Force
    }
Write-Output (Join-Path $out 'loomterm-window.exe')

