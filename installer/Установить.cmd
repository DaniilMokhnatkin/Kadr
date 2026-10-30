@echo off
rem Kadr: runs install.ps1 from this folder as administrator. Windows asks for permission once.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process powershell -Verb RunAs -ArgumentList ('-NoExit -ExecutionPolicy Bypass -File \"' + '%~dp0install.ps1' + '\"')"
