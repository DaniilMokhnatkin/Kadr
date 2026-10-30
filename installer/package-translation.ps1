# Optional model pack for a second PC with no Internet. Place its folder next to Kadr-Setup.exe.
$ErrorActionPreference='Stop'
$root=Split-Path $PSScriptRoot -Parent
$manifest=Get-Content -LiteralPath (Join-Path $PSScriptRoot 'translation-manifest.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$cache=Join-Path $root 'dist\translation-downloads'
New-Item -ItemType Directory -Path $cache -Force | Out-Null
foreach ($asset in $manifest.assets) {
    $source=Join-Path $root ('third_party\translation-downloads\'+$asset.name)
    if ((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -ne $asset.sha256) {throw "Asset mismatch: $($asset.name)"}
    $target=Join-Path $cache $asset.name
    if (-not (Test-Path -LiteralPath $target)) {New-Item -ItemType HardLink -Path $target -Target $source | Out-Null}
    if ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -ne $asset.sha256) {throw "Cache mismatch: $($asset.name)"}
}
$zip=Join-Path $root 'dist\Kadr-Offline-EN-RU.zip'
if (Test-Path -LiteralPath $zip) {Remove-Item -LiteralPath $zip -Force}
Compress-Archive -LiteralPath $cache -DestinationPath $zip -CompressionLevel NoCompression
Get-Item -LiteralPath $zip | Select-Object FullName,Length
