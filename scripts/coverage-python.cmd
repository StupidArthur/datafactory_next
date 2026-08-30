@echo off
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0coverage-python.ps1"
exit /b %errorlevel%
