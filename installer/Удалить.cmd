@echo off
rem Kadr: removes the program, its autostart and its signing certificate. Asks for permission once.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList ('-NoExit -ExecutionPolicy Bypass -File \"' + '%~dp0install.ps1' + '\" -Uninstall -RemoveCertificate')"
