# Compatibility entry point; includes current M2 and M3 checks.
param([string]$Artifacts = (Join-Path $PSScriptRoot '..\target\m2-check'))
& (Join-Path $PSScriptRoot 'check.ps1') -Artifacts $Artifacts
