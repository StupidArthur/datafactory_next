$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
    if (-not (Test-Path -LiteralPath $cargo)) { $cargo = "cargo" }
    & $cargo build --release -p df-engine
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    wails build -clean
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Copy-Item -LiteralPath "target\release\df-engine.exe" -Destination "build\bin\df-engine.exe" -Force
    Write-Host "Studio package: $root\build\bin"
} finally {
    Pop-Location
}
