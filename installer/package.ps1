<#
.SYNOPSIS
    Собирает папку и архив для установки Kadr на другой компьютер: dist\Kadr-setup и dist\Kadr-setup.zip.

.DESCRIPTION
    Внутри: kadr.exe (uiAccess, C-рантайм вшит), install.ps1, «Установить.cmd», «Удалить.cmd»
    и «ИНСТРУКЦИЯ.txt». Администратор не нужен. Запуск из корня проекта:
        cargo build --release --features uiaccess -p kadr-app
        powershell -ExecutionPolicy Bypass -File installer\package.ps1
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root 'target\release\kadr.exe'
if (-not (Select-String -Path $exe -Pattern "uiAccess=[`"']true[`"']" -Quiet)) {
    throw "$exe собран без uiAccess. Собери так: cargo build --release --features uiaccess -p kadr-app"
}
if (Select-String -Path $exe -Pattern 'VCRUNTIME140' -Quiet) {
    throw "$exe требует Visual C++ Redistributable: проверь .cargo\config.toml (crt-static)"
}
$out = Join-Path $root 'dist\Kadr-setup'
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory -Path $out | Out-Null
Copy-Item $exe $out
foreach ($name in 'install.ps1', 'Установить.cmd', 'Удалить.cmd', 'ИНСТРУКЦИЯ.txt') {
    Copy-Item (Join-Path $PSScriptRoot $name) $out
}
$zip = Join-Path $root 'dist\Kadr-setup.zip'
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $out -DestinationPath $zip
Get-Item $zip | Select-Object FullName, Length
