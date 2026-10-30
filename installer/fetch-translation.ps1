# Builds an isolated, portable local EN <-> RU translator. No system Python is required at run time.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path $PSScriptRoot -Parent
$bundle = Join-Path $root 'third_party\translation'
$cache = Join-Path $root 'third_party\translation-downloads'
New-Item -ItemType Directory -Force -Path $bundle,$cache | Out-Null
$pythonZip = Join-Path $cache 'python-3.13.17-embed-amd64.zip'
if (-not (Test-Path $pythonZip)) { Invoke-WebRequest 'https://www.python.org/ftp/python/3.13.17/python-3.13.17-embed-amd64.zip' -OutFile $pythonZip }
$runtime = Join-Path $bundle 'python'
if (-not (Test-Path (Join-Path $runtime 'python.exe'))) { Expand-Archive -LiteralPath $pythonZip -DestinationPath $runtime -Force }
$packages = Join-Path $runtime 'packages'
& py -3 -m pip install --only-binary=:all: --target $packages 'ctranslate2==4.8.4' 'sentencepiece==0.2.1' 'numpy==2.2.6' 'pyyaml==6.0.2'
if ($LASTEXITCODE -ne 0) { throw 'Could not prepare the local translation runtime' }
[IO.File]::WriteAllText((Join-Path $runtime 'python313._pth'), "python313.zip`n.`npackages`n..`nimport site`n")
foreach ($pair in 'en_ru','ru_en') {
    $archive = Join-Path $cache ("translate-" + $pair + '-1_9.argosmodel')
    if (-not (Test-Path $archive)) { Invoke-WebRequest ("https://argos-net.com/v1/translate-" + $pair + '-1_9.argosmodel') -OutFile $archive }
    $models = Join-Path $bundle 'models'
    New-Item -ItemType Directory -Path $models -Force | Out-Null
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    if (-not (Test-Path (Join-Path $models ('translate-' + $pair + '-1_9')))) { [IO.Compression.ZipFile]::ExtractToDirectory($archive,$models) }
}
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'translation-worker.py') -Destination $bundle -Force
Get-ChildItem $cache -File | Get-FileHash -Algorithm SHA256 | Select-Object Hash,Path | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $bundle 'downloads-sha256.json') -Encoding UTF8
Write-Host 'Offline translator files prepared.'
