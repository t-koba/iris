$ErrorActionPreference = "Stop"

# Consume a verified product stage only: this step builds, fetches and publishes nothing.
if ([string]::IsNullOrWhiteSpace($env:IRIS_PRODUCT_STAGE)) { throw "missing IRIS_PRODUCT_STAGE" }
if ([string]::IsNullOrWhiteSpace($env:IRIS_PRODUCT_INVENTORY)) { throw "missing IRIS_PRODUCT_INVENTORY" }
if ([string]::IsNullOrWhiteSpace($env:IRIS_PRODUCT_ZIP)) { throw "missing IRIS_PRODUCT_ZIP" }
if ([string]::IsNullOrWhiteSpace($env:IRIS_RELEASE_OUTPUT_DIR)) { throw "missing IRIS_RELEASE_OUTPUT_DIR" }

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $scriptDir "..")).Path
$stage = (Resolve-Path -LiteralPath $env:IRIS_PRODUCT_STAGE).Path
$inventoryPath = (Resolve-Path -LiteralPath $env:IRIS_PRODUCT_INVENTORY).Path
$zipPath = [IO.Path]::GetFullPath($env:IRIS_PRODUCT_ZIP)
$outputDir = [IO.Path]::GetFullPath($env:IRIS_RELEASE_OUTPUT_DIR)
if (!(Test-Path -LiteralPath $outputDir -PathType Container)) { throw "release output directory missing: $outputDir" }
$distDir = Join-Path $outputDir "Iris"
$installerPath = Join-Path $outputDir "Iris-windows-setup.exe"
$resultPath = Join-Path $outputDir "release-package-result.json"
$includePath = Join-Path $outputDir "windows-installer-files.inc"
$productResultPath = Join-Path $outputDir "product-package-result.json"

$stagePrefix = $stage.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
$distPrefix = $distDir.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
# The stage is read-only input: never let dist preparation delete or replace it.
if ($distDir -eq $stage -or $distDir.StartsWith($stagePrefix, [System.StringComparison]::OrdinalIgnoreCase) -or $stage.StartsWith($distPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "release output must be outside the product stage"
}
if (![string]::Equals((Split-Path -Parent $zipPath), $outputDir, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "product ZIP must be inside the release output directory"
}

# Constrain every field before it reaches a copy, a hash or an Inno [Files] line.
$inventory = Get-Content -LiteralPath $inventoryPath -Raw | ConvertFrom-Json
if ($null -eq $inventory -or $null -eq $inventory.files) { throw "product inventory has no files" }
$entries = @($inventory.files)
if ($entries.Count -eq 0 -or $entries.Count -gt 256) { throw "product inventory entry count out of bounds" }
$maxFileBytes = 1073741824

$expected = @{}
$ordered = @()
foreach ($item in $entries) {
    $name = [string]$item.name
    if ([string]::IsNullOrWhiteSpace($name) -or $name.Length -gt 128) { throw "invalid product inventory name" }
    if ($name -ne [IO.Path]::GetFileName($name) -or $name.Contains("/") -or $name.Contains("\") -or $name -eq "." -or $name -eq "..") {
        throw "unsafe product inventory name: $name"
    }
    # Quotes, separators, controls and non-ASCII would alter the generated Inno line.
    if ($name -match '[\x00-\x1f\x7f"\x27;]' -or $name -notmatch '^[\x20-\x7e]+$') {
        throw "unsafe product inventory name: $name"
    }
    $key = $name.ToLowerInvariant()
    if ($expected.ContainsKey($key)) { throw "duplicate product inventory name: $name" }
    $hash = ([string]$item.sha256).ToLowerInvariant()
    if ($hash -notmatch '^[0-9a-f]{64}$') { throw "invalid product inventory sha256: $name" }
    if ([string]$item.sha256 -cne $hash) { throw "product inventory sha256 must be lowercase: $name" }
    $bytes = [long]$item.bytes
    if ($bytes -le 0 -or $bytes -gt $maxFileBytes) { throw "invalid product inventory byte count: $name" }
    $expected[$key] = @{ name = $name; sha256 = $hash; bytes = $bytes }
    $ordered += $name
}

foreach ($required in @("Iris.exe", "pdfium.dll", "FFMPEG-NOTICE.md")) {
    if (!$expected.ContainsKey($required.ToLowerInvariant())) { throw "required product file missing from inventory: $required" }
}

# Compare the full stage listing, not just the inventory, so helpers cannot leak.
$staged = @(Get-ChildItem -LiteralPath $stage -Force)
if ($staged.Count -gt 256) { throw "stage listing exceeds bound" }
foreach ($file in $staged) {
    if ($file.PSIsContainer -or ($file.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "unsafe entry in product stage: $($file.Name)"
    }
    if (!$expected.ContainsKey($file.Name.ToLowerInvariant())) { throw "unlisted file in product stage: $($file.Name)" }
}
foreach ($item in $expected.Values) {
    $path = Join-Path $stage $item.name
    if (!(Test-Path -LiteralPath $path -PathType Leaf)) { throw "product inventory file missing from stage: $($item.name)" }
    $source = Get-Item -LiteralPath $path
    if ($source.PSIsContainer -or ($source.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "unsafe entry in product stage: $($item.name)"
    }
    if ($source.Length -ne $item.bytes) { throw "product file byte mismatch: $($item.name)" }
    $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
    if ($actualHash -ne $item.sha256) { throw "product file sha256 mismatch: $($item.name)" }
}

# Cargo resolves the version inherited from workspace.package; no build is run.
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
$python = Get-Command python3 -ErrorAction SilentlyContinue
if (!$python) { $python = Get-Command python -ErrorAction SilentlyContinue }
if (!$python) { throw "python is required to generate the installer file list" }

# Rebuild dist from scratch from the exact inventory, never from a stale tree.
if (Test-Path -LiteralPath $distDir) { Remove-Item -LiteralPath $distDir -Recurse -Force }
New-Item -ItemType Directory -Path $distDir | Out-Null
foreach ($name in $ordered) {
    Copy-Item -LiteralPath (Join-Path $stage $name) -Destination (Join-Path $distDir $name)
}

# GITHUB_ENV only reaches later steps, so bind the same values in this process.
$env:IRIS_PRODUCT_STAGE = $stage
$env:IRIS_PRODUCT_INVENTORY = $inventoryPath
$env:IRIS_PRODUCT_ZIP = $zipPath
Push-Location -LiteralPath $repoRoot
try {
    pwsh scripts/windows-product-package.ps1
} finally {
    Pop-Location
}
if ($LASTEXITCODE -ne 0) { throw "product ZIP assembly failed" }
if (!(Test-Path -LiteralPath $zipPath -PathType Leaf)) { throw "product ZIP not created" }
if (!(Test-Path -LiteralPath $productResultPath -PathType Leaf)) { throw "product package result missing" }
$productResult = Get-Content -LiteralPath $productResultPath -Raw | ConvertFrom-Json
if (@($productResult.entries).Count -ne $entries.Count) { throw "product package entry count mismatch" }

# The installer list is generated outside the stage and checked before compilation.
if (Test-Path -LiteralPath $includePath) { Remove-Item -LiteralPath $includePath -Force }
& $python.Source (Join-Path $scriptDir "windows-installer-files.py") $inventoryPath $includePath
if ($LASTEXITCODE -ne 0) { throw "installer file list generation failed" }
$sourceLines = @(Get-Content -LiteralPath $includePath)
if ($sourceLines.Count -ne $entries.Count) { throw "installer source count mismatch" }

if (Test-Path -LiteralPath $installerPath) { Remove-Item -LiteralPath $installerPath -Force }
& $isccPath "/DAppVersion=$appVersion" "/DFileVersion=$fileVersion" "/DInstallerFiles=$includePath" "/DSourceDir=$distDir" "/O$outputDir" (Join-Path $scriptDir "windows-installer.iss")
if ($LASTEXITCODE -ne 0) { throw "Inno Setup compilation failed" }
if (!(Test-Path -LiteralPath $installerPath -PathType Leaf)) {
    throw "Inno Setup did not produce $installerPath"
}

# Tag-only Authenticode signing. An empty flag leaves the unsigned rehearsal
# bytes untouched; the hashes below always bind the final installer bytes.
if (![string]::IsNullOrWhiteSpace($env:IRIS_WINDOWS_SIGN_PFX)) {
    & $python.Source (Join-Path $scriptDir "windows-signing.py") sign `
      --file $installerPath `
      --output (Join-Path $outputDir "windows-installer-trust.json")
    if ($LASTEXITCODE -ne 0) { throw "Windows installer signing failed" }
}

$result = @{
    status = "pass"
    zip_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $zipPath).Hash.ToLowerInvariant()
    entries = @($ordered)
    installer_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $installerPath).Hash.ToLowerInvariant()
    installer_bytes = (Get-Item -LiteralPath $installerPath).Length
    installer_source_count = $sourceLines.Count
}
Set-Content -LiteralPath $resultPath -Value ($result | ConvertTo-Json -Depth 4 -Compress) -Encoding utf8
Write-Host "Created $installerPath"
Write-Host "Created $zipPath"
