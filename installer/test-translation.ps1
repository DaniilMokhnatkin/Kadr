# Isolated installer component test. Does not install certificates, touch Program Files or run vcredist.
$ErrorActionPreference='Stop'
$tokens=$null; $errors=$null
$ast=[Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot 'install.ps1'),[ref]$tokens,[ref]$errors)
if ($errors.Count) {throw $errors[0]}
foreach ($name in 'Save-Download','Test-TranslationAsset','Get-TranslationAsset','Test-VcRuntime','Install-TranslationFiles','Install-Translation') {
    $fn=$ast.Find({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name},$true)
    if (-not $fn) {throw "install.ps1 has no $name"}
    . ([scriptblock]::Create($fn.Extent.Text))
}
$script:reports=@()
function Report([string]$line) {Write-Host $line; $script:reports+=$line}
function Start-Process {param($FilePath,$ArgumentList,[switch]$Wait,[switch]$PassThru,$WindowStyle) return [pscustomobject]@{ExitCode=0}}
function Start-Sleep {param($Seconds,$Milliseconds)}
$root=Split-Path $PSScriptRoot -Parent
$fixture=Join-Path ([IO.Path]::GetTempPath()) ('KadrTranslationTest-'+[Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixture | Out-Null
try {
    $InstallDir=Join-Path $fixture 'installed'
    New-Item -ItemType Directory -Path $InstallDir | Out-Null
    $TranslationManifest=Join-Path $fixture 'translation-manifest.json'
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'translation-manifest.json') -Destination $TranslationManifest
    $TranslationWorker=Join-Path $PSScriptRoot 'translation-worker.py'
    $TranslationCache=Join-Path $root 'dist\translation-downloads'
    function Save-Download {throw "Offline component test must not access the network"}
    $manifest=Get-Content -LiteralPath $TranslationManifest -Raw -Encoding UTF8 | ConvertFrom-Json
    foreach ($asset in $manifest.assets) {
        if ($asset.kind -eq 'wheel' -and $asset.name -notlike '*-cp313-cp313-win_amd64.whl') {throw 'Wrong Python ABI in translation manifest'}
        if (-not $asset.url) {throw "No download address for $($asset.name): a PC without the offline pack could not install it"}
        if (-not (Test-Path -LiteralPath (Join-Path $TranslationCache $asset.name))) {throw "Missing offline asset: $($asset.name)"}
    }
    $ok={param($what) if ($script:reports[-1] -ne 'ok translation') {throw "$what failed: $($script:reports[-1])"}}
    Install-Translation; & $ok 'First install'
    $runtime=Join-Path $InstallDir 'translation\python\python.exe'
    if (-not (Test-Path -LiteralPath $runtime)) {throw 'Portable runtime was not installed'}
    # Unchanged component is reused without any download or replacement.
    Install-Translation; & $ok 'Reuse'
    # A changed worker triggers a replacement; the previous copy is cleaned up once the new one works.
    $updatedWorker=Join-Path $fixture 'updated-worker.py'
    Copy-Item -LiteralPath $TranslationWorker -Destination $updatedWorker
    Add-Content -LiteralPath $updatedWorker -Value '# component update fixture' -Encoding UTF8
    $TranslationWorker=$updatedWorker
    Install-Translation; & $ok 'Update'
    if (@(Get-ChildItem $InstallDir -Directory | Where-Object Name -like 'translation-*-*').Count -ne 0) {throw 'Stage or previous copy left behind'}
    # Nothing to download from and no cache: a warning, the working component stays, no exception.
    $broken=Join-Path $fixture 'broken-manifest.json'
    $manifest.assets[0].url=$null
    $manifest.assets[0].sha256='00'
    $manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $broken -Encoding UTF8
    $TranslationManifest=$broken
    Install-Translation
    if ($script:reports[-1] -notlike 'warn translation *') {throw "A failed download must be a warning: $($script:reports[-1])"}
    if (-not (Test-Path -LiteralPath $runtime)) {throw 'A failed update removed the working component'}
    if (@(Get-ChildItem $InstallDir -Directory | Where-Object Name -like 'translation-*-*').Count -ne 0) {throw 'Failed stage left behind'}
    Write-Host 'PASS: standalone runtime extraction, SHA256 verification, real inference, safe replacement, failures as warnings.'
} finally {
    $resolved=[IO.Path]::GetFullPath($fixture)
    if (-not $resolved.StartsWith([IO.Path]::GetTempPath(),[StringComparison]::OrdinalIgnoreCase) -or (Split-Path $resolved -Leaf) -notlike 'KadrTranslationTest-*') {throw 'Invalid fixture path'}
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
