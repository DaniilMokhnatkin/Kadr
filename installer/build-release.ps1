param([Parameter(Mandatory=$true)][string]$Tag)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    if ($Tag -cnotmatch '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
        throw 'Тег должен иметь вид v1.0.0: три числа, без суффиксов.'
    }
    $metadata = (& cargo metadata --locked --no-deps --format-version 1 | Out-String) | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Не удалось прочитать версию Cargo.' }
    $app = @($metadata.packages | Where-Object { $_.name -eq 'kadr-app' })
    if ($app.Count -ne 1 -or $Tag -cne ('v' + $app[0].version)) {
        throw "Тег $Tag не совпадает с версией kadr-app в Cargo.toml."
    }
    $repo = if ($env:KADR_REPO) { $env:KADR_REPO } else { 'DaniilMokhnatkin/Kadr' }
    if ($repo -cnotmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { throw 'Неверный KADR_REPO.' }
    $env:KADR_REPO = $repo
    & cargo build --locked --release --features uiaccess -p kadr-app -p kadr-setup
    if ($LASTEXITCODE -ne 0) { throw 'Сборка завершилась ошибкой.' }
    $program = [Text.Encoding]::GetEncoding(28591).GetString([IO.File]::ReadAllBytes((Join-Path $root 'target\release\kadr.exe')))
    if (-not $program.Contains($repo)) { throw 'Адрес обновлений отсутствует в программе.' }
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'package.ps1')
    if ($LASTEXITCODE -ne 0) { throw 'Не удалось упаковать установщик.' }
    Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $root 'dist\Kadr-Setup.exe')
    Write-Host "Готово: $Tag, репозиторий $repo. Опубликуйте Kadr-Setup.exe под этим тегом."
} finally { Pop-Location }
