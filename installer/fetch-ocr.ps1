<#
.SYNOPSIS
    Скачивает PaddleOCR для Kadr в third_party\ocr: модели PP-OCRv5 (RapidAI, Apache 2.0) и
    onnxruntime.dll (Microsoft, MIT). Каждый файл сверяется по SHA-256; повторный запуск
    ничего не качает, если всё уже на месте.

.DESCRIPTION
    Источники — официальные:
      https://www.modelscope.cn/models/RapidAI/RapidOCR       (ONNX-версии моделей PaddleOCR)
      https://github.com/microsoft/onnxruntime/releases        (ONNX Runtime для Windows x64)
    Запуск из корня проекта: powershell -ExecutionPolicy Bypass -File installer\fetch-ocr.ps1
#>
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$out = Join-Path $root 'third_party\ocr'
New-Item -ItemType Directory -Path $out -Force | Out-Null

$models = 'https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/master'
$files = @(
    @{ Name = 'det.onnx'; Url = "$models/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_mobile.onnx";
       Sha = '4d97c44a20d30a81aad087d6a396b08f786c4635742afc391f6621f5c6ae78ae' },
    @{ Name = 'rec.onnx'; Url = "$models/onnx/PP-OCRv5/rec/eslav_PP-OCRv5_rec_mobile.onnx";
       Sha = '08705d6721849b1347d26187f15a5e362c431963a2a62bfff4feac578c489aab' },
    @{ Name = 'dict.txt'; Url = "$models/paddle/PP-OCRv5/rec/eslav_PP-OCRv5_rec_mobile/ppocrv5_eslav_dict.txt";
       Sha = '3e95f1581557162870cacdba5af91a4c6be2890710d395b0c3c7578e7ee5e6eb' }
)
$runtime = @{
    Url = 'https://github.com/microsoft/onnxruntime/releases/download/v1.31.1/onnxruntime-win-x64-1.31.1.zip'
    Sha = '5869e26bb25314375f2d5ef1877e3ea06b1543d6c710e6e4a1ec2f7177ab4b2c'
}

function Get-Verified([string]$url, [string]$path, [string]$sha) {
    if ((Test-Path $path) -and (Get-FileHash $path -Algorithm SHA256).Hash -eq $sha) { return }
    Write-Host "Скачиваю $url"
    $part = "$path.part"
    Invoke-WebRequest -Uri $url -OutFile $part -UseBasicParsing
    $hash = (Get-FileHash $part -Algorithm SHA256).Hash
    if ($hash -ne $sha) {
        Remove-Item $part
        throw "Контрольная сумма не совпала: $url`nожидалась $sha, получена $hash"
    }
    Move-Item $part $path -Force
}

foreach ($f in $files) {
    Get-Verified $f.Url (Join-Path $out $f.Name) $f.Sha
}

$dll = Join-Path $out 'onnxruntime.dll'
if (-not (Test-Path $dll)) {
    $zip = Join-Path $env:TEMP 'onnxruntime-win-x64-1.31.1.zip'
    Get-Verified $runtime.Url $zip $runtime.Sha
    $unpacked = Join-Path $env:TEMP 'onnxruntime-win-x64-1.31.1'
    Expand-Archive -Path $zip -DestinationPath $unpacked -Force
    Copy-Item (Join-Path $unpacked 'onnxruntime-win-x64-1.31.1\lib\onnxruntime.dll') $dll
    Copy-Item (Join-Path $unpacked 'onnxruntime-win-x64-1.31.1\LICENSE') (Join-Path $out 'LICENSE-onnxruntime.txt')
    Remove-Item $unpacked -Recurse -Force
}
Get-ChildItem $out | Select-Object Name, Length
