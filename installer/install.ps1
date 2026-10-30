<#
.SYNOPSIS
    Устанавливает Kadr с uiAccess: сертификат -> подпись -> Program Files -> автозапуск.

.DESCRIPTION
    uiAccess позволяет Kadr ловить PrtSc и показывать оверлей поверх элевированных окон
    (диспетчер задач в режиме «поверх остальных окон»). Windows выдаёт его только подписанным
    программам из Program Files.

    Скрипт:
      1. создаёт самоподписанный сертификат подписи кода (закрытый ключ не экспортируется и лежит
         в хранилище текущего пользователя) или берёт уже созданный;
      2. добавляет его публичную часть в доверенные корневые и доверенных издателей этого ПК;
      3. копирует kadr.exe в Program Files\Kadr и подписывает его;
      4. включает автозапуск и запускает Kadr с обычными правами.

    Повторный запуск обновляет установленную версию. Запускать в PowerShell от имени администратора
    из корня проекта (под той же учётной записью, под которой работаешь):
        cargo build --release --features uiaccess
        powershell -ExecutionPolicy Bypass -File installer\install.ps1

.PARAMETER Uninstall
    Удалить программу и автозапуск. Вместе с -RemoveCertificate — убрать и сертификат из всех хранилищ.
#>
[CmdletBinding()]
param(
    [string]$Exe,
    [switch]$NoAutostart,
    [switch]$Uninstall,
    [switch]$RemoveCertificate
)

$ErrorActionPreference = 'Stop'
# В Windows PowerShell 5.1 $PSScriptRoot пуст в значениях по умолчанию param() при запуске через -File,
# поэтому путь по умолчанию считаем здесь.
if (-not $Exe) {
    # In the ready package the program lies next to this script as kadr.bin: a uiAccess program
    # cannot start outside Program Files, so it must not invite a double-click there. In the
    # project it is the release build.
    $Exe = Join-Path $PSScriptRoot 'kadr.bin'
    if (-not (Test-Path $Exe)) { $Exe = Join-Path $PSScriptRoot '..\target\release\kadr.exe' }
}
Write-Verbose "exe: $Exe"
$Subject = 'CN=Kadr Local Code Signing'
$InstallDir = Join-Path $env:ProgramFiles 'Kadr'
$Target = Join-Path $InstallDir 'kadr.exe'
$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$Shortcut = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\Kadr.lnk'

function Assert-Admin {
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Нужны права администратора: запусти PowerShell «от имени администратора».'
    }
}

function Stop-Kadr {
    $running = Get-Process -Name kadr -ErrorAction SilentlyContinue
    if ($running) {
        Write-Host 'Останавливаю запущенный Kadr...'
        $running | Stop-Process -Force
        $running | Wait-Process -Timeout 5 -ErrorAction SilentlyContinue
    }
}

function Get-SigningCertificate {
    $cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
        Where-Object { $_.Subject -eq $Subject -and $_.NotAfter -gt (Get-Date) } |
        Sort-Object NotAfter -Descending |
        Select-Object -First 1
    if (-not $cert) {
        Write-Host 'Создаю сертификат подписи кода (закрытый ключ не экспортируется)...'
        $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $Subject `
            -CertStoreLocation Cert:\CurrentUser\My -KeyExportPolicy NonExportable `
            -KeyAlgorithm RSA -KeyLength 3072 -HashAlgorithm SHA256 -NotAfter (Get-Date).AddYears(5)
    }
    $public = Join-Path $env:TEMP 'kadr-signing.cer'
    try {
        Export-Certificate -Cert $cert -FilePath $public | Out-Null
        foreach ($store in 'Root', 'TrustedPublisher') {
            if (-not (Get-ChildItem "Cert:\LocalMachine\$store" | Where-Object Thumbprint -eq $cert.Thumbprint)) {
                Write-Host "Добавляю сертификат в LocalMachine\$store..."
                Import-Certificate -FilePath $public -CertStoreLocation "Cert:\LocalMachine\$store" | Out-Null
            }
        }
    }
    finally {
        Remove-Item $public -ErrorAction SilentlyContinue
    }
    $cert
}

function Remove-SigningCertificate {
    foreach ($store in 'Cert:\LocalMachine\Root', 'Cert:\LocalMachine\TrustedPublisher', 'Cert:\CurrentUser\My') {
        Get-ChildItem $store | Where-Object Subject -eq $Subject | ForEach-Object {
            Write-Host "Удаляю сертификат из $store"
            Remove-Item $_.PSPath
        }
    }
}

Assert-Admin
Stop-Kadr

if ($Uninstall) {
    Remove-ItemProperty -Path $RunKey -Name 'Kadr' -ErrorAction SilentlyContinue
    Remove-Item $Shortcut -ErrorAction SilentlyContinue
    if (Test-Path $InstallDir) { Remove-Item $InstallDir -Recurse -Force }
    if ($RemoveCertificate) { Remove-SigningCertificate }
    Write-Host 'Kadr удалён.'
    return
}

if (-not (Test-Path $Exe)) {
    throw "Не найден $Exe. Сначала собери: cargo build --release --features uiaccess"
}
$Exe = (Resolve-Path $Exe).Path
# Линкер может переписать кавычки в манифесте на одинарные — ищем оба варианта.
if (-not (Select-String -Path $Exe -Pattern "uiAccess=[`"']true[`"']" -Quiet)) {
    throw "$Exe собран без uiAccess. Собери так: cargo build --release --features uiaccess"
}

$cert = Get-SigningCertificate

New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
Copy-Item $Exe $Target -Force
# A package carried over by a messenger or the cloud is marked «from the internet»; the mark would
# make SmartScreen stop Kadr at every start.
Unblock-File -Path $Target
$signature = Set-AuthenticodeSignature -FilePath $Target -Certificate $cert -HashAlgorithm SHA256
if ($signature.Status -ne 'Valid') {
    throw "Подпись не удалась: $($signature.StatusMessage)"
}
Write-Host "Подписано: $Target"

if (-not $NoAutostart) {
    Set-ItemProperty -Path $RunKey -Name 'Kadr' -Value "`"$Target`""
    Write-Host 'Автозапуск включён.'
}

# Start menu: after «Выход» from the tray Kadr is found and started again by name.
$link = (New-Object -ComObject WScript.Shell).CreateShortcut($Shortcut)
$link.TargetPath = $Target
$link.Description = 'Kadr — скриншоты'
$link.Save()
Write-Host 'Ярлык в меню «Пуск»: Kadr.'

# Text recognition («Текст», «Перевод», «Цензура») uses Windows OCR, which has a pack per language.
# English and Russian are fetched from Windows Update if missing; without the Internet this step is
# skipped and can be repeated later by running the installer again.
foreach ($lang in 'en-US', 'ru-RU') {
    $name = "Language.OCR~~~$lang~0.0.1.0"
    try {
        $cap = Get-WindowsCapability -Online -Name $name
        if ($cap.State -eq 'Installed') {
            Write-Host "Распознавание текста ($lang): уже есть."
        } else {
            Write-Host "Скачиваю распознавание текста ($lang)… это может занять пару минут."
            Add-WindowsCapability -Online -Name $name | Out-Null
            Write-Host "Распознавание текста ($lang): установлено."
        }
    } catch {
        Write-Warning "Распознавание текста ($lang) не установлено: $($_.Exception.Message)"
    }
}

# Из этой консоли Kadr стартовал бы с правами администратора. Через explorer он запускается
# с обычными правами, и Windows выдаёт ему uiAccess.
Start-Process -FilePath explorer.exe -ArgumentList "`"$Target`""
Write-Host 'Готово. В HUD должно быть «Medium · uiAccess», лог: %LOCALAPPDATA%\Kadr\logs\kadr.log'
