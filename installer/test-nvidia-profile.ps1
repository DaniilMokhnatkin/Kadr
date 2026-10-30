# Read-only driver check. Does not run Configure(true), save a profile or install anything.
$ErrorActionPreference = 'Stop'
Add-Type -Path (Join-Path $PSScriptRoot 'nvidia-profile.cs')
foreach ($item in @(@([KadrNvidiaProfile+Profile],4116), @([KadrNvidiaProfile+Application],12296), @([KadrNvidiaProfile+Setting],12320))) {
    $actual = [Runtime.InteropServices.Marshal]::SizeOf([type]$item[0])
    if ($actual -ne $item[1]) { throw "NVAPI layout: $actual != $($item[1])" }
}
$p = [KadrNvidiaProfile+Profile]::new()
$p.name = 'Kadr screenshot utility'
$p.apps = 1
if (-not [KadrNvidiaProfile]::IsDedicated($p)) { throw 'Dedicated Kadr profile rejected' }
$p.apps = 2
if ([KadrNvidiaProfile]::IsDedicated($p)) { throw 'Shared profile accepted' }
$p.apps = 1
$p.predefined = 1
if ([KadrNvidiaProfile]::IsDedicated($p)) { throw 'Driver profile accepted' }
$p.predefined = 0
$p.name = 'Game profile'
if ([KadrNvidiaProfile]::IsDedicated($p)) { throw 'Foreign profile accepted' }
[KadrNvidiaProfile]::Configure($false)
'PASS: NVAPI layouts, scope guard and read-only driver call.'
