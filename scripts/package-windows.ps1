$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$targetDir = Join-Path $repoRoot "target/package-windows"
$distDir = Join-Path $repoRoot "dist/windows/Iris"
$zipPath = Join-Path $repoRoot "dist/windows/Iris-windows.zip"
$buildVariant = if ($env:IRIS_BUILD_VARIANT) { $env:IRIS_BUILD_VARIANT } else { "product" }

Set-Location $repoRoot
$env:CARGO_TARGET_DIR = $targetDir
$cargoArgs = @("build", "--release", "-p", "iris-ui-shell", "--bin", "Iris")
switch ($buildVariant) {
    "product" { }
    "minimal" { $cargoArgs += @("--no-default-features", "--features", "minimal") }
    default { throw "unknown IRIS_BUILD_VARIANT: $buildVariant" }
}
cargo @cargoArgs

New-Item -ItemType Directory -Force -Path $distDir | Out-Null
Copy-Item (Join-Path $targetDir "release/Iris.exe") (Join-Path $distDir "Iris.exe") -Force

if (Test-Path $zipPath) {
    Remove-Item $zipPath -Force
}

Compress-Archive -Path (Join-Path $distDir "*") -DestinationPath $zipPath -Force

Write-Host "Created $distDir\Iris.exe"
Write-Host "Created $zipPath"
