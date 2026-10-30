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
    [switch]$RemoveCertificate,
    # Graphical setup: where to report each step, and whether to skip fetching Windows OCR.
    [string]$Progress,
    [switch]$NoOcr,
    # Text recognition: «paddle» — PaddleOCR shipped with Kadr, «windows» — Windows' own OCR packs.
    [ValidateSet('paddle', 'windows')]
    [string]$Recognition = 'paddle',
    # PaddleOCR files (det.onnx, rec.onnx, dict.txt, onnxruntime.dll) as a zip.
    [string]$PaddleZip,
    # Only add PaddleOCR to an installed Kadr (when Windows' OCR could not be installed).
    [switch]$WithTranslation,
    [string]$TranslationManifest,
    [string]$TranslationWorker,
    [string]$TranslationCache,
    [switch]$PaddleOnly
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
if (-not $PaddleZip) { $PaddleZip = Join-Path $PSScriptRoot 'ocr.zip' }
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
    # Include the old portable builds renamed when their executable was locked.
    # Never match Kadr-Setup or unrelated processes.
    $running = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
        $_.ProcessName -match '^kadr(?:-running|-prev[0-9]*|-before-[a-z0-9-]+)?$'
    })
    foreach ($process in $running) {
        Write-Host "Закрываю $($process.ProcessName)..."
        try {
            if ($process.CloseMainWindow()) { [void]$process.WaitForExit(1500) }
            if (-not $process.HasExited) { $process.Kill() }
            if (-not $process.WaitForExit(10000)) {
                throw "Kadr не завершился: процесс $($process.Id). Закройте его и повторите установку."
            }
        } catch {
            # A process may have exited between enumeration and Kill.
            if (Get-Process -Id $process.Id -ErrorAction SilentlyContinue) { throw }
        }
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

# The graphical setup follows along through -Progress: one line per event, «step|ok|skip <id>»,
# «warn <id> <text>», «error <text>», «finish».
function Report([string]$line) {
    if (-not $Progress) { return }
    $bytes = [Text.Encoding]::UTF8.GetBytes($line + "`r`n")
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        $stream = $null
        try {
            $stream = [IO.FileStream]::new($Progress, [IO.FileMode]::Append, [IO.FileAccess]::Write,
                ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
            $stream.Write($bytes, 0, $bytes.Length)
            return
        } catch [IO.IOException] {
            if ($attempt -eq 39) { Write-Warning 'Не удалось обновить индикатор установки.' }
        } finally {
            if ($stream) { $stream.Dispose() }
        }
        Start-Sleep -Milliseconds 25
    }
}
trap {
    Report "error $($_.Exception.Message)"
    exit 1
}

# PaddleOCR next to Kadr, and a note of which engine was chosen (Kadr reads it on «auto»).
$OcrDir = Join-Path $InstallDir 'ocr'
$EngineNote = Join-Path $InstallDir 'ocr-engine.txt'
function Install-Paddle {
    Report 'step paddle'
    if (-not (Test-Path $PaddleZip)) { throw "Нет файлов PaddleOCR: $PaddleZip" }
    New-Item -ItemType Directory -Path $OcrDir -Force | Out-Null
    Expand-Archive -Path $PaddleZip -DestinationPath $OcrDir -Force
    foreach ($file in 'det.onnx', 'rec.onnx', 'dict.txt', 'onnxruntime.dll') {
        if (-not (Test-Path (Join-Path $OcrDir $file))) { throw "PaddleOCR распакован не полностью: нет $file" }
    }
    Get-ChildItem $OcrDir | Unblock-File
    Set-Content -LiteralPath $EngineNote -Value 'paddle' -Encoding ASCII
    Write-Host "PaddleOCR: $OcrDir"
    Report 'ok paddle'
}

# Optional local translation: download verified assets, never run pip or use a system Python.
# Windows PowerShell 5.1 on Windows 10 offers only TLS 1.0 unless told; python.org and PyPI refuse it.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

function Test-TranslationAsset($asset, [string]$path) {
    if (-not (Test-Path -LiteralPath $path)) { return $false }
    if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -eq $asset.sha256) { return $true }
    # Microsoft republishes the runtime under the same link, so its signature is what we trust.
    if ($asset.kind -eq 'vcredist') {
        $signature = Get-AuthenticodeSignature -LiteralPath $path
        return $signature.Status -eq 'Valid' -and $signature.SignerCertificate.Subject -match 'O=Microsoft Corporation'
    }
    $false
}

# A few attempts per address; curl.exe (Windows 10 1803+) when Invoke-WebRequest itself fails.
function Get-TranslationAsset($asset, [string]$file) {
    $urls = @(@($asset.url) + @($asset.mirrors) | Where-Object { $_ })
    if (-not $urls) { throw "У компонента $($asset.name) нет адреса для загрузки" }
    $part = $file + '.part'
    $last = ''
    foreach ($url in $urls) {
        for ($attempt = 1; $attempt -le 3; $attempt++) {
            Remove-Item -LiteralPath $part -Force -ErrorAction SilentlyContinue
            try {
                try {
                    Invoke-WebRequest -Uri $url -OutFile $part -UseBasicParsing
                } catch {
                    $curl = Join-Path $env:SystemRoot 'System32\curl.exe'
                    if (-not (Test-Path -LiteralPath $curl)) { throw }
                    & $curl -fsSL --retry 2 -o $part $url
                    if ($LASTEXITCODE -ne 0) { throw "$($_.Exception.Message) (curl: код $LASTEXITCODE)" }
                }
                if (Test-TranslationAsset $asset $part) {
                    Move-Item -LiteralPath $part -Destination $file -Force
                    return
                }
                $last = 'файл пришёл повреждённым (не совпала контрольная сумма)'
            } catch {
                $last = $_.Exception.Message
            }
            Start-Sleep -Seconds (2 * $attempt)
        }
    }
    Remove-Item -LiteralPath $part -Force -ErrorAction SilentlyContinue
    throw "Не удалось скачать $($asset.name): $last"
}

# The runtime ctranslate2 needs; it is built against 14.40+.
function Test-VcRuntime {
    foreach ($key in 'HKLM:\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64', 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\VisualStudio\14.0\VC\Runtimes\x64') {
        $runtime = Get-ItemProperty -LiteralPath $key -ErrorAction SilentlyContinue
        if ($runtime -and $runtime.Installed -eq 1 -and ($runtime.Major -gt 14 -or ($runtime.Major -eq 14 -and $runtime.Minor -ge 40))) { return $true }
    }
    $false
}

function Install-TranslationFiles {
    if (-not $TranslationManifest) { $TranslationManifest=Join-Path $PSScriptRoot 'translation-manifest.json' }
    if (-not $TranslationWorker) { $TranslationWorker=Join-Path $PSScriptRoot 'translation-worker.py' }
    foreach ($needed in $TranslationManifest, $TranslationWorker) {
        if (-not (Test-Path -LiteralPath $needed)) { throw "В установщике нет файла $([IO.Path]::GetFileName($needed))" }
    }
    $manifest=Get-Content -LiteralPath $TranslationManifest -Raw -Encoding UTF8 | ConvertFrom-Json
    if (-not $manifest.assets) { throw 'Список компонентов перевода пуст' }
    $installed=Join-Path $InstallDir 'translation'
    $previousManifest=Join-Path $installed 'install-manifest.json'
    $previousWorker=Join-Path $installed 'translation-worker.py'
    if ((Test-Path -LiteralPath $previousManifest) -and (Test-Path -LiteralPath $previousWorker) -and
        (Test-Path -LiteralPath (Join-Path $installed 'python\python.exe')) -and
        (Test-Path -LiteralPath (Join-Path $installed 'models\translate-en_ru-1_9\model\model.bin')) -and
        (Test-Path -LiteralPath (Join-Path $installed 'models\translate-ru_en-1_9\model\model.bin')) -and
        (Get-FileHash -LiteralPath $previousManifest).Hash -eq (Get-FileHash -LiteralPath $TranslationManifest).Hash -and
        (Get-FileHash -LiteralPath $previousWorker).Hash -eq (Get-FileHash -LiteralPath $TranslationWorker).Hash) {
        return
    }
    # Leftovers of earlier runs: a stage that failed half-way, a version replaced before.
    Get-ChildItem -LiteralPath $InstallDir -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like 'translation-new-*' -or $_.Name -like 'translation-old-*' } |
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    $stage=Join-Path $InstallDir ('translation-new-'+[Guid]::NewGuid().ToString('N'))
    try {
        $downloads=Join-Path ([IO.Path]::GetDirectoryName($TranslationManifest)) 'translation-downloads'
        $runtime=Join-Path $stage 'python'
        New-Item -ItemType Directory -Path $runtime,$downloads -Force | Out-Null
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $ProgressPreference='SilentlyContinue'
        foreach ($asset in $manifest.assets) {
            if (-not $asset.name -or [IO.Path]::GetFileName($asset.name) -ne $asset.name) {throw 'Некорректный пакет перевода'}
            if ($asset.kind -eq 'vcredist' -and (Test-VcRuntime)) { continue }
            $file=Join-Path $downloads $asset.name
            if ($TranslationCache -and -not (Test-TranslationAsset $asset $file)) {
                $cached=Join-Path $TranslationCache $asset.name
                if (Test-TranslationAsset $asset $cached) { Copy-Item -LiteralPath $cached -Destination $file -Force }
            }
            if (-not (Test-TranslationAsset $asset $file)) { Get-TranslationAsset $asset $file }
            if ($asset.kind -eq 'vcredist') {
                $signature=Get-AuthenticodeSignature -LiteralPath $file
                if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'Microsoft Corporation') {throw 'Неверная подпись Microsoft Visual C++ Runtime'}
                $prerequisite=Start-Process -FilePath $file -ArgumentList '/install','/quiet','/norestart' -WindowStyle Hidden -Wait -PassThru
                # 1638 and 5100: a newer runtime is already there; 1641 and 3010: a restart finishes it.
                if ($prerequisite.ExitCode -notin @(0,1638,1641,3010,5100)) {throw "Не удалось установить Microsoft Visual C++ Runtime: код $($prerequisite.ExitCode)"}
                continue
            }
            $destination=switch ($asset.kind) {
                'python' {$runtime}
                'wheel' {Join-Path $runtime 'packages'}
                'model' {Join-Path $stage 'models'}
                default {throw "Неизвестный тип пакета перевода: $($asset.kind)"}
            }
            New-Item -ItemType Directory -Path $destination -Force | Out-Null
            [IO.Compression.ZipFile]::ExtractToDirectory($file,$destination)
        }
        [IO.File]::WriteAllText((Join-Path $runtime 'python313._pth'), "python313.zip`n.`npackages`n..`nimport site`n")
        Copy-Item -LiteralPath $TranslationWorker -Destination (Join-Path $stage 'translation-worker.py') -Force
        Copy-Item -LiteralPath $TranslationManifest -Destination (Join-Path $stage 'install-manifest.json') -Force
        Get-ChildItem -LiteralPath $stage -Recurse -File | Unblock-File -ErrorAction SilentlyContinue
        # Test a real inference before replacing a previously installed component. UTF-8 without
        # a BOM: the console's code page (or a BOM) would garble the request on some systems.
        $OutputEncoding=[Text.UTF8Encoding]::new($false)
        $probe=@('{"text":"Hello","to":"ru"}' | & (Join-Path $runtime 'python.exe') -I -u (Join-Path $stage 'translation-worker.py'))
        if ($LASTEXITCODE -ne 0) {throw "Офлайн-переводчик не запустился (код $LASTEXITCODE)"}
        $line=$probe | Where-Object { $_ -and $_.Trim().StartsWith('{') } | Select-Object -Last 1
        if (-not $line) {throw 'Офлайн-переводчик ничего не ответил на проверке'}
        $reply=$line | ConvertFrom-Json
        if ($reply.error -or -not $reply.text) {throw "Офлайн-переводчик не прошёл проверку: $($reply.error)"}
        # A worker left running from the previous version would keep the folder locked.
        Get-Process python -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -and $_.Path.StartsWith($installed, [StringComparison]::OrdinalIgnoreCase) } |
            Stop-Process -Force -ErrorAction SilentlyContinue
        $old=Join-Path $InstallDir ('translation-old-'+[Guid]::NewGuid().ToString('N'))
        if (Test-Path -LiteralPath $installed) {Move-Item -LiteralPath $installed -Destination $old}
        try {Move-Item -LiteralPath $stage -Destination $installed} catch {
            if (Test-Path -LiteralPath $old) {Move-Item -LiteralPath $old -Destination $installed}
            throw
        }
        if (Test-Path -LiteralPath $old) { Remove-Item -LiteralPath $old -Recurse -Force -ErrorAction SilentlyContinue }
    } finally {
        if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue }
    }
}

# Translation is an extra: when it cannot be installed, Kadr still is, and the setup says why.
function Install-Translation {
    Report 'step translation'
    try {
        Install-TranslationFiles
        Report 'ok translation'
    } catch {
        $why = $_.Exception.Message
        Write-Warning "Офлайн-перевод не установлен: $why"
        Report "warn translation $why. Пока переводит Google; повторите установку позже, чтобы добавить офлайн-перевод."
    }
}

Report 'step prepare'
Assert-Admin

if ($PaddleOnly) {
    # Kadr keeps running: it picks PaddleOCR up at its next recognition.
    if (-not (Test-Path $Target)) { throw 'Kadr ещё не установлен.' }
    Report 'ok prepare'
    Install-Paddle
    Report 'finish'
    return
}

if ($Uninstall) {
    Stop-Kadr
    Remove-ItemProperty -Path $RunKey -Name 'Kadr' -ErrorAction SilentlyContinue
    Remove-Item $Shortcut -ErrorAction SilentlyContinue
    if (Test-Path $InstallDir) { Remove-Item $InstallDir -Recurse -Force }
    if ($RemoveCertificate) { Remove-SigningCertificate }
    Write-Host 'Kadr удалён.'
    Report 'finish'
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
Report 'ok prepare'

Report 'step cert'
$cert = Get-SigningCertificate
Report 'ok cert'

Report 'step copy'
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
# Prepare and verify the new executable before touching the working installation.
$Staged = Join-Path $InstallDir ('kadr-new-' + [Guid]::NewGuid().ToString('N') + '.exe')
$Backup = Join-Path $InstallDir ('kadr-old-' + [Guid]::NewGuid().ToString('N') + '.exe')
try {
    Copy-Item -LiteralPath $Exe -Destination $Staged -Force
    Unblock-File -LiteralPath $Staged
    $signature = Set-AuthenticodeSignature -FilePath $Staged -Certificate $cert -HashAlgorithm SHA256
    if ($signature.Status -ne 'Valid') { throw "Подпись не удалась: $($signature.StatusMessage)" }
    Stop-Kadr
    # Atomic replacement keeps the old executable intact if replacement fails.
    if (Test-Path -LiteralPath $Target) {
        [IO.File]::Replace($Staged, $Target, $Backup)
    } else {
        [IO.File]::Move($Staged, $Target)
    }
} finally {
    if (Test-Path -LiteralPath $Staged) { Remove-Item -LiteralPath $Staged -Force }
}
Write-Host "Установлено: $Target"
Report 'ok copy'

Report 'step autostart'
if (-not $NoAutostart) {
    Set-ItemProperty -Path $RunKey -Name 'Kadr' -Value "`"$Target`""
    Write-Host 'Автозапуск включён.'
} else {
    Remove-ItemProperty -Path $RunKey -Name 'Kadr' -ErrorAction SilentlyContinue
}
# Start menu: after «Выход» from the tray Kadr is found and started again by name.
$link = (New-Object -ComObject WScript.Shell).CreateShortcut($Shortcut)
$link.TargetPath = $Target
$link.Description = 'Kadr — скриншоты'
$link.Save()
Write-Host 'Ярлык в меню «Пуск»: Kadr.'
Report 'ok autostart'

if ($Recognition -eq 'paddle') {
    Install-Paddle
} else {
    Set-Content -LiteralPath $EngineNote -Value 'windows' -Encoding ASCII
    # Windows' OCR has a pack per language. English and Russian are fetched from Windows Update if
    # missing; without the Internet this step fails and the setup offers PaddleOCR instead.
    foreach ($lang in 'en-US', 'ru-RU') {
        $id = 'ocr-' + $lang.Substring(0, 2)
        if ($NoOcr) { Report "skip $id"; continue }
        Report "step $id"
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
            Report "ok $id"
        } catch {
            Write-Warning "Распознавание текста ($lang) не установлено: $($_.Exception.Message)"
            Report "warn $id $($_.Exception.Message)"
        }
    }
}

# Из этой консоли Kadr стартовал бы с правами администратора. Через explorer он запускается
# с обычными правами, и Windows выдаёт ему uiAccess.
if ($WithTranslation) { Install-Translation }
# A dedicated per-app driver profile; optional and never changes global/game settings.
try {
    $nvidiaSource = Join-Path $PSScriptRoot 'nvidia-profile.cs'
    if (Test-Path -LiteralPath $nvidiaSource) {
        Add-Type -Path $nvidiaSource
        Write-Host ([KadrNvidiaProfile]::Configure($true))
    }
} catch {
    Write-Warning "Профиль NVIDIA для Kadr не применён: $($_.Exception.Message)"
    Report "warn autostart Профиль NVIDIA для Kadr не применён: $($_.Exception.Message)"
}
Report 'step launch'
Start-Process -FilePath explorer.exe -ArgumentList "`"$Target`""
Report 'ok launch'
if (Test-Path -LiteralPath $Backup) { Remove-Item -LiteralPath $Backup -Force -ErrorAction SilentlyContinue }
Write-Host 'Готово. В HUD должно быть «Medium · uiAccess», лог: %LOCALAPPDATA%\Kadr\logs\kadr.log'
Report 'finish'
