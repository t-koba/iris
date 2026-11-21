$ErrorActionPreference = "Stop"

# Assemble one rehearsal ZIP from files that match a bounded, exact-SHA product
# inventory. The script only reads the staged copies; it builds, acquires and
# publishes nothing.
if ([string]::IsNullOrWhiteSpace($env:IRIS_PRODUCT_STAGE)) { throw "missing IRIS_PRODUCT_STAGE" }
if ([string]::IsNullOrWhiteSpace($env:IRIS_PRODUCT_INVENTORY)) { throw "missing IRIS_PRODUCT_INVENTORY" }
if ([string]::IsNullOrWhiteSpace($env:IRIS_PRODUCT_ZIP)) { throw "missing IRIS_PRODUCT_ZIP" }

$stage = (Resolve-Path -LiteralPath $env:IRIS_PRODUCT_STAGE).Path
$inventoryPath = (Resolve-Path -LiteralPath $env:IRIS_PRODUCT_INVENTORY).Path
$zipPath = [IO.Path]::GetFullPath($env:IRIS_PRODUCT_ZIP)
$zipDir = Split-Path -Parent $zipPath
$resultPath = Join-Path $zipDir "product-package-result.json"
if (!(Test-Path -LiteralPath $zipDir -PathType Container)) { throw "ZIP output directory missing: $zipDir" }
$stagePrefix = $stage.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
if ($zipPath.StartsWith($stagePrefix, [System.StringComparison]::OrdinalIgnoreCase) -or $resultPath.StartsWith($stagePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "product package output must be outside the stage"
}

$inventory = Get-Content -LiteralPath $inventoryPath -Raw | ConvertFrom-Json
if ($null -eq $inventory -or $null -eq $inventory.files) { throw "product inventory has no files" }
$entries = @($inventory.files)
if ($entries.Count -eq 0 -or $entries.Count -gt 256) { throw "product inventory entry count out of bounds" }

$expected = @{}
$ordered = @()
foreach ($item in $entries) {
    $name = [string]$item.name
    if ([string]::IsNullOrWhiteSpace($name) -or $name.Length -gt 128) { throw "invalid product inventory name" }
    if ($name -ne [IO.Path]::GetFileName($name) -or $name.Contains("/") -or $name.Contains("\") -or $name -eq "." -or $name -eq "..") {
        throw "unsafe product inventory name: $name"
    }
    $key = $name.ToLowerInvariant()
    if ($expected.ContainsKey($key)) { throw "duplicate product inventory name: $name" }
    $hash = ([string]$item.sha256).ToLowerInvariant()
    if ($hash -notmatch '^[0-9a-f]{64}$') { throw "invalid product inventory sha256: $name" }
    $bytes = [long]$item.bytes
    if ($bytes -le 0) { throw "invalid product inventory byte count: $name" }
    $expected[$key] = @{ name = $name; sha256 = $hash; bytes = $bytes }
    $ordered += $name
}

$staged = @(Get-ChildItem -LiteralPath $stage -File)
if ($staged.Count -gt 256) { throw "stage listing exceeds bound" }
foreach ($file in $staged) {
    if (!$expected.ContainsKey($file.Name.ToLowerInvariant())) { throw "unlisted file in product stage: $($file.Name)" }
}
foreach ($required in @("Iris.exe", "pdfium.dll")) {
    if (!$expected.ContainsKey($required.ToLowerInvariant())) { throw "required product file missing from inventory: $required" }
}
foreach ($item in $expected.Values) {
    $path = Join-Path $stage $item.name
    if (!(Test-Path -LiteralPath $path -PathType Leaf)) { throw "product inventory file missing from stage: $($item.name)" }
    if ((Get-Item -LiteralPath $path).Length -ne $item.bytes) { throw "product file byte mismatch: $($item.name)" }
    $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
    if ($actualHash -ne $item.sha256) { throw "product file sha256 mismatch: $($item.name)" }
}

if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
if (Test-Path -LiteralPath $resultPath) { Remove-Item -LiteralPath $resultPath -Force }
$sources = @($ordered | ForEach-Object { Join-Path $stage $_ })
Compress-Archive -LiteralPath $sources -DestinationPath $zipPath -CompressionLevel Optimal -Force
if (!(Test-Path -LiteralPath $zipPath -PathType Leaf)) { throw "product ZIP not created" }

$result = @{
    zip_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $zipPath).Hash.ToLowerInvariant()
    entries = @($ordered)
}
Set-Content -LiteralPath $resultPath -Value ($result | ConvertTo-Json -Depth 4 -Compress) -Encoding utf8
Write-Host "assembled product rehearsal ZIP: $zipPath"
