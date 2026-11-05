$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$targetDir = Join-Path $repoRoot "target/package-windows"
$distDir = Join-Path $repoRoot "dist/windows/Iris"
$zipPath = Join-Path $repoRoot "dist/windows/Iris-windows.zip"
$installerPath = Join-Path $repoRoot "dist/windows/Iris-windows-setup.exe"
$buildVariant = if ($env:IRIS_BUILD_VARIANT) { $env:IRIS_BUILD_VARIANT } else { "product" }

# Inno Setup may be installed without adding its compiler to PATH.
$iscc = Get-Command "ISCC.exe" -ErrorAction SilentlyContinue
if ($iscc) {
    $isccPath = $iscc.Source
} else {
    $isccPath = if (${env:ProgramFiles(x86)}) {
        Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6/ISCC.exe"
    }
    if (!$isccPath -or !(Test-Path -LiteralPath $isccPath -PathType Leaf)) {
        throw "Inno Setup 6 is required: install it or add ISCC.exe to PATH"
    }
}

Set-Location $repoRoot
# Cargo resolves the version inherited from workspace.package for us.
$metadataJson = cargo metadata --no-deps --format-version 1 --locked
if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed" }
$metadata = $metadataJson | ConvertFrom-Json
$appPackage = @($metadata.packages | Where-Object { $_.name -eq "iris-ui-shell" })
if ($appPackage.Count -ne 1) { throw "expected one iris-ui-shell package" }
$appVersion = $appPackage[0].version
# Windows executable version resources require a numeric version, unlike AppVersion.
if ($appVersion -notmatch '^(\d+\.\d+\.\d+)(?:[-+].*)?$') {
    throw "invalid Iris package version: $appVersion"
}
$fileVersion = $Matches[1]
$env:CARGO_TARGET_DIR = $targetDir
$cargoArgs = @("build", "--locked", "--release", "-p", "iris-ui-shell", "--bin", "Iris")
switch ($buildVariant) {
    "product" { }
    "minimal" { $cargoArgs += @("--no-default-features", "--features", "minimal") }
    default { throw "unknown IRIS_BUILD_VARIANT: $buildVariant" }
}
cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

New-Item -ItemType Directory -Force -Path $distDir | Out-Null
Copy-Item (Join-Path $targetDir "release/Iris.exe") (Join-Path $distDir "Iris.exe") -Force

if (Test-Path $zipPath) {
    Remove-Item $zipPath -Force
}

Compress-Archive -Path (Join-Path $distDir "*") -DestinationPath $zipPath -Force

# Keep installer output outside the existing portable ZIP's source directory.
if (Test-Path $installerPath) {
    Remove-Item $installerPath -Force
}
& $isccPath "/DAppVersion=$appVersion" "/DFileVersion=$fileVersion" "/DSourceDir=$distDir" "/O$(Split-Path -Parent $installerPath)" (Join-Path $scriptDir "windows-installer.iss")
if ($LASTEXITCODE -ne 0) { throw "Inno Setup compilation failed" }
if (!(Test-Path -LiteralPath $installerPath -PathType Leaf)) {
    throw "Inno Setup did not produce $installerPath"
}

Write-Host "Created $installerPath"
Write-Host "Created $distDir\Iris.exe"
Write-Host "Created $zipPath"
