#Requires -Version 7.0
param(
    [string]$Exe,
    [string]$NativeHost,
    [string]$Fixture,
    [string]$ShellPath = 'C:\Program Files\PowerShell\7\pwsh.exe',
    [string]$EvidenceRoot,
    [int]$TimeoutSeconds = 420,
    [string]$Target = 'x86_64-pc-windows-msvc',
    [ValidateSet('core', 'extended', 'all')]
    [string]$Suite = 'core',
    [switch]$SkipBuild,
    [switch]$Offline
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repo 'apps/loomterm-cli/Cargo.toml'
$artifacts = Join-Path $repo 'target/cli-native'
if (!$EvidenceRoot) { $EvidenceRoot = Join-Path $repo 'docs/local/w1-02/m1/native' }
New-Item -ItemType Directory -Force -Path $artifacts, $EvidenceRoot | Out-Null
$EvidenceRoot = [IO.Path]::GetFullPath($EvidenceRoot)

if (!$SkipBuild) {
    # Reuse the caller's pinned Rust/native build environment; never download tools.
    Push-Location (Split-Path -Parent $manifest)
    try {
        $cargoArgs = @('test', '--test', 'loomterm_native', '--target', $Target,
            '--no-run', '--locked', '--message-format=json')
        if ($Offline) { $cargoArgs += '--offline' }
        $messages = & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) { throw 'Native host/application build failed' }
    } finally {
        Pop-Location
    }
    $build = @($messages | ForEach-Object {
        try { $_ | ConvertFrom-Json } catch { }
    })
    if (!$NativeHost) {
        $NativeHost = ($build | Where-Object { $_.reason -eq 'compiler-artifact' -and $_.target.name -eq 'loomterm_native' -and $_.executable } | Select-Object -Last 1).executable
    }
    if (!$Exe) {
        $Exe = ($build | Where-Object { $_.reason -eq 'compiler-artifact' -and $_.target.name -eq 'loomterm' -and $_.executable } | Select-Object -Last 1).executable
    }
    if (!$Fixture) {
        $Fixture = Join-Path $artifacts 'cli-engine.exe'
        $fixtureSource = Join-Path $PSScriptRoot 'fixtures/cli-engine.c'
        & cl.exe /nologo /std:c17 /W4 /WX /O2 /MT $fixtureSource "/Fe:$Fixture" "/Fo:$(Join-Path $artifacts 'cli-engine.obj')"
        if ($LASTEXITCODE -ne 0) { throw 'Controlled query fixture build failed' }
    }
}
foreach ($candidate in @($Exe, $NativeHost, $Fixture, $ShellPath)) {
    if (!$candidate -or !(Test-Path -LiteralPath $candidate -PathType Leaf)) {
        throw "Required native input is missing: $candidate"
    }
}

$run = [Guid]::NewGuid().ToString('N')
$stdout = Join-Path $EvidenceRoot "controller-$run.stdout.jsonl"
$stderr = Join-Path $EvidenceRoot "controller-$run.stderr.log"
$cancel = Join-Path $EvidenceRoot "controller-$run.cancel"
$controller = Join-Path $PSScriptRoot 'fixtures/cli-native.py'
$start = [Diagnostics.ProcessStartInfo]::new()
$start.FileName = (Get-Command python).Source
$start.WorkingDirectory = $repo
$start.UseShellExecute = $false
$start.CreateNoWindow = $true
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
foreach ($arg in @($controller, '--exe', $Exe, '--host', $NativeHost, '--fixture', $Fixture,
        '--shell', $ShellPath, '--evidence', $EvidenceRoot, '--deadline-seconds', "$TimeoutSeconds", '--cancel-file', $cancel, '--suite', $Suite)) {
    $start.ArgumentList.Add($arg)
}
$process = [Diagnostics.Process]::new()
$process.StartInfo = $start
if (!$process.Start()) { throw 'Controller did not start' }
$outRead = $process.StandardOutput.ReadToEndAsync()
$errRead = $process.StandardError.ReadToEndAsync()
$deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds + 20)
while (!$process.WaitForExit(500)) {
    if ([DateTime]::UtcNow -ge $deadline) {
        [IO.File]::WriteAllText($cancel, 'outer watchdog requested bounded normal cleanup')
        if (!$process.WaitForExit(45000)) {
            throw "Controller did not finish cleanup; exact controller PID $($process.Id), evidence $EvidenceRoot"
        }
    }
}
[IO.File]::WriteAllText($stdout, $outRead.GetAwaiter().GetResult())
[IO.File]::WriteAllText($stderr, $errRead.GetAwaiter().GetResult())
Get-Content -LiteralPath $stdout
if ($process.ExitCode -ne 0) { throw "Native TUI regression failed; evidence $EvidenceRoot" }
