# Safe regression checks: no installation, certificate changes or real process termination.
$ErrorActionPreference = 'Stop'
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot 'install.ps1'), [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw $errors[0] }
foreach ($name in 'Report', 'Stop-Kadr') {
    $function = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name }, $true)
    . ([scriptblock]::Create($function.Extent.Text))
}
$folder = Join-Path ([IO.Path]::GetTempPath()) ('KadrInstallerTest-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $folder | Out-Null
try {
    $Progress = Join-Path $folder 'progress.txt'
    [IO.File]::WriteAllText($Progress, '')
    $reader = [IO.FileStream]::new($Progress, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    try {
        1..100 | ForEach-Object { Report "ok event-$_" }
    } finally { $reader.Dispose() }
    if ((Get-Content -LiteralPath $Progress).Count -ne 100) { throw 'Reports lost while reader was open' }

    # Simulate a short-lived exclusive lock held by another thread.
    $lock = [IO.FileStream]::new($Progress, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    $worker = [PowerShell]::Create()
    try {
        [void]$worker.AddScript('param($stream) Start-Sleep -Milliseconds 150; $stream.Dispose()').AddArgument($lock)
        $pending = $worker.BeginInvoke()
        Report 'finish'
        [void]$worker.EndInvoke($pending)
    } finally { $lock.Dispose(); $worker.Dispose() }
    if ((Get-Content -LiteralPath $Progress)[-1] -ne 'finish') { throw 'Report did not recover after a file lock' }

    # Replace Get-Process locally: Stop-Kadr must not touch the actual desktop.
    $script:processes = @()
    foreach ($name in 'kadr', 'Kadr-running', 'kadr-prev2', 'Kadr-before-Motion', 'Kadr-Setup', 'explorer') {
        $process = [pscustomobject]@{ ProcessName = $name; Id = 1; HasExited = $false }
        $process | Add-Member ScriptMethod CloseMainWindow { return $false }
        $process | Add-Member ScriptMethod Kill { $this.HasExited = $true }
        $process | Add-Member ScriptMethod WaitForExit { param($timeout) return $this.HasExited }
        $script:processes += $process
    }
    function Get-Process { param($Id, $ErrorAction) return $script:processes }
    Stop-Kadr
    foreach ($process in $script:processes) {
        $expected = $process.ProcessName -notin @('Kadr-Setup', 'explorer')
        if ($process.HasExited -ne $expected) { throw "Incorrect process match: $($process.ProcessName)" }
    }
    'PASS: shared progress, transient lock retry, old process names, unrelated processes preserved.'
} finally {
    # The only deleted path is the unique temporary fixture created above.
    if ((Split-Path $folder -Leaf) -notlike 'KadrInstallerTest-*') { throw 'Invalid fixture path' }
    Remove-Item -LiteralPath $folder -Recurse -Force
}
