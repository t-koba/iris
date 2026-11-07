# Prototype-only test app staging: default-feature Iris + FFmpeg DLL closure + pinned PDFium.
# Test-only; never a release payload. Fails closed. Uses the actual prototype
# VCPKG_INSTALLED_DIR and the custom dynamic triplet with VCPKGRS_DYNAMIC=1.
$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "../..")).Path
$pinsPath = Join-Path $scriptDir "pins.json"
$appDir = Join-Path $repoRoot "target/prototype-windows-app"
$pdfiumDir = Join-Path $repoRoot "target/prototype-windows-pdfium"
$ffmpegDir = Join-Path $repoRoot "target/prototype-windows-ffmpeg"
$ffmpegSourceEvidence = Join-Path $ffmpegDir "ffmpeg-source.json"
$ffmpegBuildEvidence = Join-Path $ffmpegDir "ffmpeg-build.json"
$ffmpegBuildconf = Join-Path $ffmpegDir "ffmpeg-buildconf.txt"
$ffmpegVersion = Join-Path $ffmpegDir "ffmpeg-version.txt"
$overlayEvidence = Join-Path $ffmpegDir "overlay.json"
$installLog = Join-Path $ffmpegDir "vcpkg-install.log"
$toolPathFile = Join-Path $ffmpegDir "ffmpeg-tool.path"

$pins = Get-Content -Raw -LiteralPath $pinsPath | ConvertFrom-Json
if ($pins.ffmpeg.commit -ne "db8537d8cc7962bd3d07254d6ceff21d888817d4") { throw "pins.json ffmpeg commit drifted" }
if ($pins.vcpkg.baseline -ne "b8dfe957ec8b5c0ef5c4b31eea594b8486af744f") { throw "pins.json vcpkg baseline drifted" }

# The exact-commit FFmpeg gate + build must have passed before anything is staged.
foreach ($req in @($ffmpegSourceEvidence, $ffmpegBuildEvidence, $ffmpegBuildconf, $overlayEvidence, $installLog, $toolPathFile)) {
    if (!(Test-Path -LiteralPath $req -PathType Leaf)) {
        throw "refusing to stage: missing FFmpeg build evidence $req; run build-windows-ffmpeg-prototype.ps1 first"
    }
}
$gate = Get-Content -Raw -LiteralPath $ffmpegSourceEvidence | ConvertFrom-Json
# Only pinned, proven upstream supersessions are valid non-applications.
# source-bundle.py rechecks their pristine source contexts and manifest below.
$failed = @($gate.patches | Where-Object {
    $_.applies -ne $true -and -not (
        $_.omitted -eq $true -and $_.applied -eq $false -and
        $_.evidence.ffmpeg_commit -ceq $pins.ffmpeg.commit -and (
            ($_.patch -ceq $pins.vcpkg.ffmpeg_msvc_utf8_patch.name -and
             $_.evidence.baseline_sha256 -ceq $pins.vcpkg.ffmpeg_msvc_utf8_patch.sha256 -and
             $_.evidence.source_sha256 -cmatch '^[0-9a-f]{64}$') -or
            ($_.patch -ceq $pins.vcpkg.ffmpeg_unstable_swscale_patch.name -and
             $_.evidence.baseline_sha256 -ceq $pins.vcpkg.ffmpeg_unstable_swscale_patch.sha256 -and
             $_.evidence.vcpkg_baseline -ceq $pins.vcpkg.baseline -and
             $_.evidence.source_sha256.configure -ceq $pins.vcpkg.ffmpeg_unstable_swscale_patch.source_sha256.configure -and
             $_.evidence.source_sha256.'libswscale/format.c' -ceq $pins.vcpkg.ffmpeg_unstable_swscale_patch.source_sha256.'libswscale/format.c' -and
             $_.evidence.source_sha256.'libswscale/graph.c' -ceq $pins.vcpkg.ffmpeg_unstable_swscale_patch.source_sha256.'libswscale/graph.c')
        )
    )
})
if ($failed.Count -gt 0) { throw "refusing to stage: $($failed.Count) vcpkg patches do not apply to the pinned FFmpeg commit" }
$build = Get-Content -Raw -LiteralPath $ffmpegBuildEvidence | ConvertFrom-Json
if ($build.ffmpeg.commit -ne "db8537d8cc7962bd3d07254d6ceff21d888817d4") { throw "refusing to stage: ffmpeg-build.json commit mismatch" }
if ($build.vcpkg.baseline -ne "b8dfe957ec8b5c0ef5c4b31eea594b8486af744f") { throw "refusing to stage: ffmpeg-build.json baseline mismatch" }
if ($build.triplet -ne "x64-windows-prototype-dynamic") { throw "refusing to stage: ffmpeg-build.json triplet must be x64-windows-prototype-dynamic" }

# Resolve the actual prototype install root (isolated; never release paths).
if ([string]::IsNullOrWhiteSpace($env:VCPKG_ROOT)) { throw "VCPKG_ROOT is required for Rust discovery" }
$installedDir = Join-Path (Resolve-Path $env:VCPKG_ROOT).Path "installed"
if ($env:VCPKG_INSTALLED_DIR -and [IO.Path]::GetFullPath($env:VCPKG_INSTALLED_DIR) -ne [IO.Path]::GetFullPath($installedDir)) {
    throw "VCPKG_INSTALLED_DIR must equal VCPKG_ROOT/installed for Rust discovery"
}
$effectiveTriplet = $env:VCPKG_TARGET_TRIPLET
if ([string]::IsNullOrWhiteSpace($effectiveTriplet)) { $effectiveTriplet = $env:IRIS_PROTOTYPE_TRIPLET }
if ([string]::IsNullOrWhiteSpace($effectiveTriplet)) { $effectiveTriplet = "x64-windows-prototype-dynamic" }
if ($effectiveTriplet -ne "x64-windows-prototype-dynamic") { throw "prototype install triplet must be x64-windows-prototype-dynamic, got '$effectiveTriplet'" }
$vcpkgBin = Join-Path $installedDir "$effectiveTriplet/bin"
if (!(Test-Path -LiteralPath $vcpkgBin -PathType Container)) {
    Write-Host "VCPKG_INSTALLED_DIR layout:"
    Get-ChildItem -Recurse -Directory -LiteralPath $installedDir -ErrorAction SilentlyContinue | Select-Object -First 20 FullName | ForEach-Object { Write-Host $_.FullName }
    throw "FFmpeg DLL closure missing: $vcpkgBin not found under isolated VCPKG_INSTALLED_DIR=$installedDir"
}
$vcpkgShare = Join-Path $installedDir "$effectiveTriplet/share/ffmpeg"
Write-Host "prototype FFmpeg bin: $vcpkgBin"

# Same env for the product build: dynamic linkage against the prototype install.
$env:VCPKGRS_DYNAMIC = "1"
$env:VCPKG_TARGET_TRIPLET = $effectiveTriplet
$env:VCPKG_DEFAULT_TRIPLET = $effectiveTriplet
$env:VCPKG_INSTALLED_DIR = $installedDir
if ($env:VCPKGRS_TRIPLET -and $env:VCPKGRS_TRIPLET -ne $effectiveTriplet) { throw "VCPKGRS_TRIPLET mismatch" }
$env:VCPKGRS_TRIPLET = $effectiveTriplet
Write-Host "Rust vcpkg discovery: $env:VCPKG_ROOT/installed/$env:VCPKGRS_TRIPLET (dynamic)"

Set-Location $repoRoot
# Product defaults only: no --no-default-features, no minimal variant.
# The helper applies only the pinned binding override, verifies the temporary
# dependency graph, and restores the original Cargo.lock bytes in finally.
& python (Join-Path $scriptDir "prototype-binding-build.py")
if ($LASTEXITCODE -ne 0) { throw "default-feature Iris build failed" }
$irisExe = Join-Path $repoRoot "target/release/Iris.exe"
if (!(Test-Path -LiteralPath $irisExe -PathType Leaf)) { throw "default-feature Iris.exe missing: $irisExe" }

if (Test-Path -LiteralPath $appDir) { Remove-Item -Recurse -Force -LiteralPath $appDir }
New-Item -ItemType Directory -Force -Path $appDir | Out-Null
Copy-Item -LiteralPath $irisExe -Destination (Join-Path $appDir "Iris.exe") -Force

# Complete FFmpeg DLL closure: every DLL from the prototype triplet bin.
$dlls = @(Get-ChildItem -LiteralPath $vcpkgBin -Filter "*.dll" -File)
if ($dlls.Count -eq 0) { throw "FFmpeg DLL closure is empty in $vcpkgBin" }
foreach ($d in ($dlls | Sort-Object Name)) { Copy-Item -LiteralPath $d.FullName -Destination $appDir -Force }
Write-Host "staged $($dlls.Count) FFmpeg DLLs from $vcpkgBin"

$pdfiumDll = Join-Path $pdfiumDir "pdfium.dll"
if (!(Test-Path -LiteralPath $pdfiumDll -PathType Leaf)) {
    throw "pinned pdfium.dll missing; run stage-windows-pdfium-prototype.ps1 first"
}
Copy-Item -LiteralPath $pdfiumDll -Destination (Join-Path $appDir "pdfium.dll") -Force

# Source / license / notice material plus tested config evidence (copies only; no licensing conclusion).
$docDir = Join-Path $appDir "doc"
New-Item -ItemType Directory -Force -Path $docDir | Out-Null
# Manifest identity is bound by the successful build record. Verify every input
# before copying and again in app/doc; missing material is never optional.
$bundleRecord = $build.source_bundle_manifest
if (!$bundleRecord -or !$bundleRecord.sha256 -or !$bundleRecord.bytes) { throw "refusing to stage: source bundle manifest identity missing" }
& python (Join-Path $scriptDir "source-bundle.py") stage (Join-Path $ffmpegDir "source-bundle") (Join-Path $docDir "ffmpeg-source-bundle") $bundleRecord.sha256 $bundleRecord.bytes
if ($LASTEXITCODE -ne 0) { throw "refusing to stage: source bundle missing or hash/byte mismatch" }
Copy-Item -LiteralPath (Join-Path $ffmpegDir "binding-override.json") -Destination (Join-Path $docDir "binding-override.json") -Force
Copy-Item -LiteralPath $pinsPath -Destination (Join-Path $docDir "pins.json") -Force
Copy-Item -LiteralPath (Join-Path $scriptDir "vcpkg.json") -Destination (Join-Path $docDir "vcpkg.json") -Force
Copy-Item -LiteralPath (Join-Path $scriptDir "vcpkg-configuration.json") -Destination (Join-Path $docDir "vcpkg-configuration.json") -Force
Copy-Item -LiteralPath (Join-Path $scriptDir "triplets/x64-windows-prototype-dynamic.cmake") -Destination (Join-Path $docDir "x64-windows-prototype-dynamic.cmake") -Force
Copy-Item -LiteralPath (Join-Path $scriptDir "windows-system-dll-allowlist.txt") -Destination (Join-Path $docDir "windows-system-dll-allowlist.txt") -Force
$vcAllow = Join-Path $scriptDir "windows-vc-redist-allowlist.txt"
if (Test-Path -LiteralPath $vcAllow -PathType Leaf) { Copy-Item -LiteralPath $vcAllow -Destination (Join-Path $docDir "windows-vc-redist-allowlist.txt") -Force }
Copy-Item -LiteralPath $ffmpegSourceEvidence -Destination (Join-Path $docDir "ffmpeg-source.json") -Force
Copy-Item -LiteralPath $ffmpegBuildEvidence -Destination (Join-Path $docDir "ffmpeg-build.json") -Force
Copy-Item -LiteralPath $overlayEvidence -Destination (Join-Path $docDir "overlay.json") -Force
Copy-Item -LiteralPath $ffmpegBuildconf -Destination (Join-Path $docDir "ffmpeg-buildconf.txt") -Force
if (Test-Path -LiteralPath $ffmpegVersion -PathType Leaf) { Copy-Item -LiteralPath $ffmpegVersion -Destination (Join-Path $docDir "ffmpeg-version.txt") -Force }
if (Test-Path -LiteralPath $toolPathFile -PathType Leaf) { Copy-Item -LiteralPath $toolPathFile -Destination (Join-Path $docDir "ffmpeg-tool.path") -Force }
Copy-Item -LiteralPath (Join-Path $pdfiumDir "PROVENANCE.json") -Destination (Join-Path $docDir "pdfium-PROVENANCE.json") -Force
if (Test-Path -LiteralPath (Join-Path $pdfiumDir "pdfium-members.txt") -PathType Leaf) {
    Copy-Item -LiteralPath (Join-Path $pdfiumDir "pdfium-members.txt") -Destination (Join-Path $docDir "pdfium-members.txt") -Force
}
$copyrightSrc = Join-Path $vcpkgShare "copyright"
if (Test-Path -LiteralPath $copyrightSrc -PathType Leaf) { Copy-Item -LiteralPath $copyrightSrc -Destination (Join-Path $docDir "ffmpeg-copyright") -Force }
# FFmpeg source license files from the exact-commit checkout (if present).
$ffmpegSourceDir = Join-Path $ffmpegDir "ffmpeg-source"
foreach ($name in @("COPYING.LGPLv2.1", "COPYING.LGPLv3", "COPYING.GPLv2", "COPYING.GPLv3", "LICENSE.md", "COPYING")) {
    $src = Join-Path $ffmpegSourceDir $name
    if (Test-Path -LiteralPath $src -PathType Leaf) { Copy-Item -LiteralPath $src -Destination (Join-Path $docDir ("ffmpeg-source-" + $name)) -Force }
}
$pdfiumLicense = Join-Path $pdfiumDir "doc/LICENSE"
if (Test-Path -LiteralPath $pdfiumLicense -PathType Leaf) { Copy-Item -LiteralPath $pdfiumLicense -Destination (Join-Path $docDir "pdfium-LICENSE") -Force }
$pdfiumVersion = Join-Path $pdfiumDir "doc/VERSION"
if (Test-Path -LiteralPath $pdfiumVersion -PathType Leaf) { Copy-Item -LiteralPath $pdfiumVersion -Destination (Join-Path $docDir "pdfium-VERSION") -Force }
$pdfiumArgs = Join-Path $pdfiumDir "doc/args.gn"
if (Test-Path -LiteralPath $pdfiumArgs -PathType Leaf) { Copy-Item -LiteralPath $pdfiumArgs -Destination (Join-Path $docDir "pdfium-args.gn") -Force }
$pdfiumLicenses = Join-Path $pdfiumDir "doc/licenses"
if (Test-Path -LiteralPath $pdfiumLicenses -PathType Container) {
    $destLic = Join-Path $docDir "pdfium-licenses"
    if (Test-Path -LiteralPath $destLic) { Remove-Item -Recurse -Force -LiteralPath $destLic }
    Copy-Item -Recurse -Path (Join-Path $pdfiumLicenses "*") -Destination $destLic -Force
}

$files = @(Get-ChildItem -LiteralPath $appDir -File | ForEach-Object {
    [ordered]@{
        file = $_.Name
        bytes = $_.Length
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant()
    }
})
$docFiles = @(Get-ChildItem -Recurse -File -LiteralPath $docDir | ForEach-Object {
    $rel = $_.FullName.Substring($docDir.Length).TrimStart('\', '/')
    [ordered]@{
        file = ("doc/" + $rel)
        bytes = $_.Length
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant()
    }
})
[ordered]@{
    schema = 1
    note = "Prototype-only test app dir. Default-feature Iris plus pinned FFmpeg DLL closure and pinned PDFium. Source/license/notice copies and tested config evidence only; no licensing conclusion. Not a release."
    ffmpeg_commit = $pins.ffmpeg.commit
    vcpkg_baseline = $pins.vcpkg.baseline
    triplet = $effectiveTriplet
    installed_dir = $installedDir
    vcpkgrs_dynamic = "1"
    files = @($files)
    doc_files = @($docFiles)
} | ConvertTo-Json -Depth 6 | Out-File -Encoding utf8 (Join-Path $appDir "PROTOTYPE-APP.json")
Write-Host "staged prototype app in $appDir"
