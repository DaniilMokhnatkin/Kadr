param([string]$Artifacts = (Join-Path $PSScriptRoot '..\target\checks'))
$ErrorActionPreference = 'Stop'
$project = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Artifacts = [IO.Path]::GetFullPath($Artifacts)
New-Item -ItemType Directory -Force -Path $Artifacts | Out-Null
$resultPath = Join-Path $Artifacts 'result.txt'
'RUNNING: Kadr background checks' | Set-Content -LiteralPath $resultPath
$previousArtifacts = $env:KADR_TEST_ARTIFACTS
$previousIncremental = $env:CARGO_INCREMENTAL
Push-Location $project
try {
    $env:KADR_TEST_ARTIFACTS = $Artifacts
    $env:CARGO_INCREMENTAL = '0'
    # Tests may create hidden windows; they never show or activate them, send SendInput, or change clipboard.
    & cargo test -p kadr-doc -p kadr-editor -p kadr-render -p kadr-store -p kadr-win -p kadr-export -- --test-threads=1 --skip clipboard::
    if ($LASTEXITCODE -ne 0) { throw 'Core checks failed' }
    & cargo test -p kadr-app -- --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw 'Kadr integration failed' }
    & cargo clippy --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Clippy failed' }
    "Kadr passed. No desktop input was sent.`nArtifacts: $Artifacts" | Tee-Object -FilePath $resultPath
} catch {
    "FAILED: $($_.Exception.Message)" | Set-Content -LiteralPath $resultPath
    throw
} finally {
    $env:KADR_TEST_ARTIFACTS = $previousArtifacts
    $env:CARGO_INCREMENTAL = $previousIncremental
    Pop-Location
}
