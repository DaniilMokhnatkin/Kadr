<#
.SYNOPSIS
    Собирает папку и архив для установки Kadr на другой компьютер: dist\Kadr-setup и dist\Kadr-setup.zip.

.DESCRIPTION
    Внутри: kadr.bin (это kadr.exe: uiAccess, C-рантайм вшит; .bin — чтобы его не запускали из папки), install.ps1, «Установить.cmd», «Удалить.cmd»
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
# PaddleOCR (fetched by installer\fetch-ocr.ps1) as one zip for the install script.
$ocr = Join-Path $root 'third_party\ocr'
foreach ($file in 'det.onnx', 'rec.onnx', 'dict.txt', 'onnxruntime.dll') {
    if (-not (Test-Path (Join-Path $ocr $file))) { throw "Нет PaddleOCR: запусти installer\fetch-ocr.ps1" }
}
Copy-Item (Join-Path $PSScriptRoot 'NOTICE-ocr.txt') $ocr
$ocrZip = Join-Path $root 'dist\ocr.zip'
if (Test-Path $ocrZip) { Remove-Item $ocrZip -Force }
Compress-Archive -Path (Join-Path $ocr '*') -DestinationPath $ocrZip -CompressionLevel Optimal
Copy-Item $exe (Join-Path $out 'kadr.bin')
Copy-Item $ocrZip $out
foreach ($name in 'translation-manifest.json', 'translation-worker.py', 'nvidia-profile.cs', 'install.ps1', 'Установить.cmd', 'Удалить.cmd', 'ИНСТРУКЦИЯ.txt') {
    Copy-Item (Join-Path $PSScriptRoot $name) $out
}
$zip = Join-Path $root 'dist\Kadr-setup.zip'
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $out -DestinationPath $zip
Get-Item $zip | Select-Object FullName, Length

# The friendly one-file setup: kadr-setup.exe with kadr.exe, install.ps1 and PaddleOCR carried at
# its end, `[setup][kadr.exe][install.ps1][ocr.zip][exe u64][script u64][ocr u64]KADRSET2`.
$setup = Join-Path $root 'target\release\kadr-setup.exe'
if (-not (Test-Path $setup)) {
    throw "Нет $setup. Собери: cargo build --release -p kadr-setup"
}
$single = Join-Path $root 'dist\Kadr-Setup.exe'
$script = [IO.File]::ReadAllBytes((Join-Path $PSScriptRoot 'install.ps1'))
$program = [IO.File]::ReadAllBytes($exe)
$paddle = [IO.File]::ReadAllBytes($ocrZip)
$file = [IO.File]::Create($single)
try {
    foreach ($part in @([IO.File]::ReadAllBytes($setup), $program, $script, $paddle,
            [BitConverter]::GetBytes([UInt64]$program.Length), [BitConverter]::GetBytes([UInt64]$script.Length),
            [BitConverter]::GetBytes([UInt64]$paddle.Length), [Text.Encoding]::ASCII.GetBytes('KADRSET2'))) {
        $file.Write($part, 0, $part.Length)
    }
} finally {
    $file.Dispose()
}
Get-Item $single | Select-Object FullName, Length
