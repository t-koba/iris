# Prototype-only pinned Windows PDFium staging. Test-only; never a release payload.
# Fails closed. Validates archive members before extraction and requires the
# complete LICENSE/VERSION/args.gn/licenses closure.
$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "../..")).Path
$pinsPath = Join-Path $scriptDir "pins.json"
$stageDir = Join-Path $repoRoot "target/prototype-windows-pdfium"
New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

$pins = Get-Content -Raw -LiteralPath $pinsPath | ConvertFrom-Json
$release = $pins.pdfium.release
$asset = $pins.pdfium.asset
$bytes = [long]$pins.pdfium.bytes
$sha = $pins.pdfium.sha256
$url = $pins.pdfium.url

if ($release -ne "chromium/8138") { throw "pins.json pdfium release drifted: $release" }
if ($asset -ne "pdfium-win-x64.tgz") { throw "pins.json pdfium asset drifted: $asset" }
if ($bytes -ne 4031929) { throw "pins.json pdfium bytes drifted: $bytes" }
if ($sha -ne "8d48973e3eb472dda0f5f2435e595ac043f9f2219dd71883392f549445afa8a5") { throw "pins.json pdfium sha256 drifted" }

$tarball = Join-Path $stageDir $asset
function Test-Pin($path) {
    if (!(Test-Path -LiteralPath $path -PathType Leaf)) { return $false }
    if ((Get-Item -LiteralPath $path).Length -ne $bytes) { return $false }
    return ((Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant() -eq $sha.ToLowerInvariant())
}

if ($env:IRIS_PDFIUM_TARBALL) {
    if (!(Test-Pin $env:IRIS_PDFIUM_TARBALL)) { throw "IRIS_PDFIUM_TARBALL does not match the pinned $asset size/sha256" }
    Copy-Item -LiteralPath $env:IRIS_PDFIUM_TARBALL -Destination $tarball -Force
} elseif (!(Test-Pin $tarball)) {
    Write-Host "downloading $url"
    Invoke-WebRequest -Uri $url -OutFile "$tarball.part" -UseBasicParsing
    if (!(Test-Pin "$tarball.part")) {
        Remove-Item -Force -LiteralPath "$tarball.part" -ErrorAction SilentlyContinue
        throw "downloaded $asset does not match the pinned size/sha256"
    }
    Move-Item -Force -LiteralPath "$tarball.part" -Destination $tarball
}
Write-Host "verified $asset ($bytes bytes, sha256 $sha)"

# --- Validate archive members BEFORE extraction (fail closed, no guessing) ---
if (!(Get-Command tar -ErrorAction SilentlyContinue)) { throw "tar is required to inspect the pinned PDFium archive" }
$membersPath = Join-Path $stageDir "pdfium-members.txt"
tar -tzf $tarball 2>&1 | Out-File -Encoding utf8 $membersPath
if ($LASTEXITCODE -ne 0) { throw "pdfium archive member listing failed: $tarball" }
$members = @(Get-Content -LiteralPath $membersPath | ForEach-Object { $_.Trim() } | Where-Object { $_ -ne "" } | Sort-Object -Unique)
if ($members.Count -eq 0) { throw "pdfium archive has no members: $tarball" }
Write-Host "pdfium archive members: $($members.Count)"
$members | Select-Object -First 30 | ForEach-Object { Write-Host "  member: $_" }
# Refuse path traversal / absolute members.
foreach ($m in $members) {
    if ($m.StartsWith("/") -or $m.StartsWith("\") -or ($m.Length -ge 2 -and $m[1] -eq ":")) {
        throw "pdfium archive has absolute member '$m'; refusing extraction"
    }
    if ($m -match "(^|/)\.\.(/|$)") { throw "pdfium archive has traversal member '$m'; refusing extraction" }
}
function Find-Member($predicate, $label) {
    $hit = @($members | Where-Object $predicate)
    return $hit
}
# Required DLL (exact layout probe, no guessing beyond these two).
$dllMembers = @($members | Where-Object { ($_ -eq "bin/pdfium.dll") -or ($_ -eq "pdfium.dll") })
if ($dllMembers.Count -eq 0) {
    Write-Host "UNEXPECTED pdfium layout: no bin/pdfium.dll or pdfium.dll"
    throw "pinned $asset has an unexpected layout (no bin/pdfium.dll or pdfium.dll); members recorded in pdfium-members.txt"
}
# Required LICENSE / VERSION / args.gn / licenses/ closure.
$licenseMembers = @($members | Where-Object { ($_ -eq "LICENSE") -or ($_ -eq "bin/LICENSE") -or ($_ -like "*/LICENSE") })
$versionMembers = @($members | Where-Object { ($_ -eq "VERSION") -or ($_ -eq "bin/VERSION") -or ($_ -like "*/VERSION") })
$argsMembers = @($members | Where-Object { ($_ -eq "args.gn") -or ($_ -like "*/args.gn") })
$licensesMembers = @($members | Where-Object { ($_ -eq "licenses") -or ($_ -like "licenses/*") -or ($_ -like "*/licenses/*") })
$missing = @()
if ($licenseMembers.Count -eq 0) { $missing += "LICENSE" }
if ($versionMembers.Count -eq 0) { $missing += "VERSION" }
if ($argsMembers.Count -eq 0) { $missing += "args.gn" }
if ($licensesMembers.Count -eq 0) { $missing += "licenses/" }
if ($missing.Count -gt 0) {
    Write-Host ("pdfium archive is missing required members: " + ($missing -join ", "))
    Write-Host "first 100 members:"
    $members | Select-Object -First 100 | ForEach-Object { Write-Host "  $_" }
    throw ("pinned $asset layout differs from the required LICENSE/VERSION/args.gn/licenses closure (missing: {0}); see pdfium-members.txt" -f ($missing -join ", "))
}
Write-Host ("pdfium required members ok: dll={0} license={1} version={2} args.gn={3} licenses={4} files" -f $dllMembers.Count, $licenseMembers.Count, $versionMembers.Count, $argsMembers.Count, $licensesMembers.Count)

$extract = Join-Path $stageDir "extract"
if (Test-Path -LiteralPath $extract) { Remove-Item -Recurse -Force -LiteralPath $extract }
New-Item -ItemType Directory -Force -Path $extract | Out-Null
tar -xzf $tarball -C $extract
if ($LASTEXITCODE -ne 0) { throw "pdfium archive extraction failed" }

# Resolve extracted files from the validated member names (normalize separators).
function Resolve-Extracted($member) {
    $rel = $member -replace "/", [System.IO.Path]::DirectorySeparatorChar
    $rel = $rel.TrimStart([System.IO.Path]::DirectorySeparatorChar)
    return (Join-Path $extract $rel)
}
$dll = $null
foreach ($m in $dllMembers) {
    $candidate = Resolve-Extracted $m
    if (Test-Path -LiteralPath $candidate -PathType Leaf) { $dll = $candidate; break }
}
if (!$dll) { throw "validated pdfium.dll member did not extract; refusing to proceed" }
$dllSha = (Get-FileHash -Algorithm SHA256 -LiteralPath $dll).Hash.ToLowerInvariant()
$dllBytes = (Get-Item -LiteralPath $dll).Length
Write-Host "pdfium.dll: $dllBytes bytes, sha256 $dllSha"

$outDoc = Join-Path $stageDir "doc"
if (Test-Path -LiteralPath $outDoc) { Remove-Item -Recurse -Force -LiteralPath $outDoc }
New-Item -ItemType Directory -Force -Path $outDoc | Out-Null
Copy-Item -LiteralPath $dll -Destination (Join-Path $stageDir "pdfium.dll") -Force

function Copy-Validated($memberList, $docName) {
    $src = $null
    foreach ($m in $memberList) {
        $candidate = Resolve-Extracted $m
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { $src = $candidate; break }
    }
    if (!$src) { throw "validated member for $docName did not extract" }
    Copy-Item -LiteralPath $src -Destination (Join-Path $outDoc $docName) -Force
    $h = (Get-FileHash -Algorithm SHA256 -LiteralPath $src).Hash.ToLowerInvariant()
    $b = (Get-Item -LiteralPath $src).Length
    Write-Host "$docName : $b bytes, sha256 $h (from $src)"
    return [ordered]@{ file = $docName; bytes = $b; sha256 = $h }
}
$licenseRecord = Copy-Validated $licenseMembers "LICENSE"
$versionRecord = Copy-Validated $versionMembers "VERSION"
$argsRecord = Copy-Validated $argsMembers "args.gn"

# Complete licenses/ closure: copy every validated licenses/ member preserving hierarchy.
$licensesOut = Join-Path $outDoc "licenses"
New-Item -ItemType Directory -Force -Path $licensesOut | Out-Null
$licensesRecords = @()
foreach ($m in $licensesMembers) {
    $src = Resolve-Extracted $m
    if (Test-Path -LiteralPath $src -PathType Container) { continue }
    if (!(Test-Path -LiteralPath $src -PathType Leaf)) { continue }
    $rel = $m -replace "/", [System.IO.Path]::DirectorySeparatorChar
    $dest = Join-Path $licensesOut $rel
    # Flatten any leading `licenses/` prefix duplication is fine; preserve hierarchy under doc/licenses.
    $leafRel = $rel
    if ($leafRel -like "licenses*") { $leafRel = $leafRel.Substring("licenses".Length).TrimStart([System.IO.Path]::DirectorySeparatorChar) }
    if ([string]::IsNullOrWhiteSpace($leafRel)) { continue }
    $dest = Join-Path $licensesOut $leafRel
    $destDir = Split-Path -Parent $dest
    New-Item -ItemType Directory -Force -Path $destDir | Out-Null
    Copy-Item -LiteralPath $src -Destination $dest -Force
    $licensesRecords += [ordered]@{
        member = $m
        bytes = (Get-Item -LiteralPath $src).Length
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $src).Hash.ToLowerInvariant()
    }
}
if ($licensesRecords.Count -eq 0) { throw "licenses/ closure validated but no files extracted; refusing to proceed" }
Write-Host ("licenses/ closure: {0} files" -f $licensesRecords.Count)

$provenance = [ordered]@{
    schema = 1
    archive = [ordered]@{ asset = $asset; bytes = $bytes; sha256 = $sha.ToLowerInvariant(); url = $url; release = $release; members = $members.Count }
    runtime = [ordered]@{ file = "pdfium.dll"; bytes = $dllBytes; sha256 = $dllSha }
    docs = [ordered]@{ license = $licenseRecord; version = $versionRecord; args_gn = $argsRecord; licenses_files = $licensesRecords.Count }
    note = "Prototype-only Windows PDFium runtime; bundled beside the test app. No licensing conclusion."
}
$provenance | ConvertTo-Json -Depth 6 | Out-File -Encoding utf8 (Join-Path $stageDir "PROVENANCE.json")
"PDFium prototype runtime staged: $asset -> pdfium.dll ($dllSha)" | Out-File -Encoding utf8 (Join-Path $stageDir "PROVENANCE")
Write-Host "staged prototype pdfium.dll in $stageDir"
