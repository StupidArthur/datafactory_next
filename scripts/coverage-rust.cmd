@echo off
setlocal
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat" -arch=x64 >nul
if errorlevel 1 exit /b %errorlevel%
if exist "%~dp0..\target\llvm-cov-target" rmdir /s /q "%~dp0..\target\llvm-cov-target"
"%USERPROFILE%\.cargo\bin\cargo.exe" +nightly-x86_64-pc-windows-msvc llvm-cov --workspace --all-features --json --output-path "%~dp0..\target\rust-coverage.json"
if errorlevel 1 exit /b %errorlevel%
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0coverage-rust-gate.ps1" "%~dp0..\target\rust-coverage.json"
exit /b %errorlevel%
