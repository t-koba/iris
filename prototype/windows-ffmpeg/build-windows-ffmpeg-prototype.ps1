# Prototype-only pinned FFmpeg exact-commit build via a test-only vcpkg overlay.
# Test-only; never a release payload. Fails closed. Does not change release.yml
# or packaging scripts. Never silently falls back to FFmpeg 9.0.2.
$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "../..")).Path
$pinsPath = Join-Path $scriptDir "pins.json"
$manifestPath = Join-Path $scriptDir "vcpkg.json"
$configPath = Join-Path $scriptDir "vcpkg-configuration.json"
$tripletsDir = Join-Path $scriptDir "triplets"
$customTripletFile = Join-Path $tripletsDir "x64-windows-prototype-dynamic.cmake"
$evidenceDir = Join-Path $repoRoot "target/prototype-windows-ffmpeg"
New-Item -ItemType Directory -Force -Path $evidenceDir | Out-Null

$pins = Get-Content -Raw -LiteralPath $pinsPath | ConvertFrom-Json
$ffmpegCommit = $pins.ffmpeg.commit
$vcpkgBaseline = $pins.vcpkg.baseline
$ffmpegRepo = $pins.ffmpeg.repository
$pinsTriplet = $pins.triplet

if ($ffmpegCommit -ne "db8537d8cc7962bd3d07254d6ceff21d888817d4") { throw "pins.json ffmpeg commit drifted: $ffmpegCommit" }
if ($vcpkgBaseline -ne "b8dfe957ec8b5c0ef5c4b31eea594b8486af744f") { throw "pins.json vcpkg baseline drifted: $vcpkgBaseline" }
if ($ffmpegRepo -ne "https://github.com/FFmpeg/FFmpeg.git") { throw "pins.json ffmpeg repository drifted: $ffmpegRepo" }
if ($pinsTriplet -ne "x64-windows") { throw "pins.json triplet base must stay x64-windows (dynamic): $pinsTriplet" }
if (!(Test-Path -LiteralPath $customTripletFile -PathType Leaf)) { throw "custom dynamic triplet missing: $customTripletFile" }
$tripletContent = Get-Content -Raw -LiteralPath $customTripletFile
if ($tripletContent -notmatch "VCPKG_LIBRARY_LINKAGE\s+dynamic") { throw "custom triplet must set dynamic library linkage" }
if ($tripletContent -notmatch "VCPKG_CRT_LINKAGE\s+dynamic") { throw "custom triplet must set dynamic CRT linkage" }
if ($tripletContent -match "(?i)static") {
    # The only allowed `static` tokens are the fail-closed guards above.
    if ($tripletContent -notmatch "FATAL_ERROR") { throw "custom triplet must not select static linkage" }
}

# Manifest and configuration must carry the same baseline pin.
$manifestRaw = Get-Content -Raw -LiteralPath $manifestPath
$configRaw = Get-Content -Raw -LiteralPath $configPath
if ($manifestRaw -notmatch [regex]::Escape($vcpkgBaseline)) { throw "vcpkg.json does not carry the pinned baseline $vcpkgBaseline" }
if ($configRaw -notmatch [regex]::Escape($vcpkgBaseline)) { throw "vcpkg-configuration.json does not carry the pinned baseline $vcpkgBaseline" }

# LGPL-only/default library features plus exactly one test-only executable
# feature (`ffmpeg`) for deterministic smoke-video generation. Refuse gpl,
# nonfree, or a broad `all` feature anywhere in the manifest.
foreach ($token in @("gpl", "nonfree")) {
    if ($manifestRaw -match ('(?i)"' + $token + '"')) { throw "prototype vcpkg manifest must not request feature '$token' (LGPL-only)" }
}
if ($manifestRaw -match '(?i)"all"') { throw "prototype vcpkg manifest must not request broad feature 'all'" }
$manifest = $manifestRaw | ConvertFrom-Json
foreach ($dep in @($manifest.dependencies)) {
    if ($dep -is [string]) { continue }
    $features = @()
    if ($null -ne $dep.features) { $features = @($dep.features) }
    foreach ($f in $features) {
        if ($f -match '^(?i)(gpl|nonfree|all)$') { throw "prototype must not request vcpkg feature '$f' (LGPL-only, no broad all)" }
    }
    if ($dep.name -eq "ffmpeg") {
        if ($dep.'default-features' -ne $true) { throw "prototype ffmpeg dependency must use default-features only" }
        # Only the test-only executable feature is allowed as an extra.
        foreach ($f in $features) {
            if ($f.ToLowerInvariant() -ne "ffmpeg") { throw "prototype ffmpeg allows only test-only extra feature 'ffmpeg' (got '$f'); library features stay default LGPL-only" }
        }
        if (@($features).Count -gt 1) { throw "prototype ffmpeg must request at most the single test-only 'ffmpeg' executable feature" }
    }
}

# Effective install triplet is the isolated custom dynamic triplet (never static).
$effectiveTriplet = $env:VCPKG_TARGET_TRIPLET
if ([string]::IsNullOrWhiteSpace($effectiveTriplet)) { $effectiveTriplet = $env:IRIS_PROTOTYPE_TRIPLET }
if ([string]::IsNullOrWhiteSpace($effectiveTriplet)) { $effectiveTriplet = "x64-windows-prototype-dynamic" }
if ($effectiveTriplet -ne "x64-windows-prototype-dynamic") { throw "prototype install triplet must be x64-windows-prototype-dynamic (dynamic), got '$effectiveTriplet'" }
foreach ($name in @("VCPKG_TARGET_TRIPLET", "VCPKG_DEFAULT_TRIPLET", "VCPKGRS_TRIPLET")) {
    $value = [Environment]::GetEnvironmentVariable($name)
    if (![string]::IsNullOrWhiteSpace($value) -and $value -ne "x64-windows-prototype-dynamic") {
        throw "$name must be x64-windows-prototype-dynamic for the prototype (dynamic), got '$value'"
    }
}
if ([Environment]::GetEnvironmentVariable("VCPKGRS_DYNAMIC") -ne "1") {
    throw "VCPKGRS_DYNAMIC must be 1 for dynamic FFmpeg linkage (workflow supplies it)"
}
$env:VCPKGRS_TRIPLET = $effectiveTriplet

# Tool versions are logged; assert the measured OS/VS bounds in the workflow gate. Exact minor pins remain unconfigured until a Windows runner is measured.
foreach ($tool in @("git", "cmake", "ninja", "python")) {
    if (!(Get-Command $tool -ErrorAction SilentlyContinue)) { throw "required tool is missing: $tool" }
}
Write-Host "--- tool versions ---"
$gitVer = ((git --version) 2>&1 | Out-String).Trim()
$cmakeVer = ((cmake --version 2>&1 | Select-Object -First 1 | Out-String)).Trim()
$ninjaVer = ((ninja --version) 2>&1 | Out-String).Trim()
$pythonVer = ((python --version) 2>&1 | Out-String).Trim()
Write-Host $gitVer
Write-Host $cmakeVer
Write-Host $ninjaVer
Write-Host $pythonVer
$cl = Get-Command "cl.exe" -ErrorAction SilentlyContinue
if ($cl) {
    Write-Host ("cl: " + $cl.Source)
    $clOut = ((& $cl.Source 2>&1 | Select-Object -First 3 | Out-String)).Trim()
    Write-Host $clOut
} else {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (!(Test-Path -LiteralPath $vswhere -PathType Leaf)) { throw "pinned MSVC toolchain is not available: neither cl.exe nor vswhere.exe found" }
    Write-Host "--- vswhere instances ---"
    & $vswhere -latest -property installationPath,installationVersion,catalog_productDisplayVersion
    $vsPath = ((& $vswhere -latest -property installationPath | Select-Object -First 1 | Out-String)).Trim()
    if ([string]::IsNullOrWhiteSpace($vsPath)) { throw "pinned MSVC toolchain is not available: vswhere found no instance" }
    Write-Host "VS instance: $vsPath"
    throw "cl.exe is not on PATH; the workflow MSVC setup step must run first (see README)"
}
$sdkNote = [Environment]::GetEnvironmentVariable("WindowsSDKVersion")
if ($sdkNote) { Write-Host "WindowsSDKVersion: $sdkNote" } else { Write-Host "WindowsSDKVersion: (not set; using toolchain default, logged for evidence)" }

if ([string]::IsNullOrWhiteSpace($env:VCPKG_ROOT) -or !(Test-Path -LiteralPath $env:VCPKG_ROOT -PathType Container)) {
    throw "VCPKG_ROOT is required and must point at the pinned vcpkg checkout (workflow supplies it)"
}
$vcpkgRoot = (Resolve-Path $env:VCPKG_ROOT).Path
# vcpkg-rs 0.2.15 searches VCPKG_ROOT/installed, not VCPKG_INSTALLED_DIR.
$installedDir = Join-Path $vcpkgRoot "installed"
if ($env:VCPKG_INSTALLED_DIR -and [IO.Path]::GetFullPath($env:VCPKG_INSTALLED_DIR) -ne [IO.Path]::GetFullPath($installedDir)) {
    throw "VCPKG_INSTALLED_DIR must equal VCPKG_ROOT/installed for Rust discovery"
}
$env:VCPKG_INSTALLED_DIR = $installedDir
Write-Host "prototype install: $installedDir/$effectiveTriplet (VCPKGRS_TRIPLET=$env:VCPKGRS_TRIPLET)"
Write-Host "VCPKG_ROOT: $vcpkgRoot"
$vcpkgExe = Join-Path $vcpkgRoot "vcpkg.exe"
if (!(Test-Path -LiteralPath $vcpkgExe -PathType Leaf)) { throw "vcpkg.exe not found under VCPKG_ROOT: $vcpkgExe (bootstrap step must run first)" }
$vcpkgHead = ((git -C $vcpkgRoot rev-parse HEAD) | Out-String).Trim()
Write-Host "vcpkg HEAD: $vcpkgHead"
if ($vcpkgHead -ne $vcpkgBaseline) { throw "VCPKG_ROOT HEAD $vcpkgHead != pinned baseline $vcpkgBaseline; refusing to build (no fallback)" }
git -C $vcpkgRoot cat-file -e "$vcpkgBaseline^{commit}"
if ($LASTEXITCODE -ne 0) { throw "pinned vcpkg baseline $vcpkgBaseline is not present in VCPKG_ROOT" }
& $vcpkgExe --version
if ($LASTEXITCODE -ne 0) { throw "vcpkg --version failed" }

# The derivation/patch check must use the exact tracked baseline files.
git -C $vcpkgRoot diff --exit-code $vcpkgBaseline -- ports/ffmpeg scripts/cmake/vcpkg_from_git.cmake
if ($LASTEXITCODE -ne 0) { throw "baseline FFmpeg port/helper has local changes" }

# Baseline ffmpeg port must exist; its feature set is inspected so the
# test-only executable feature is validated (fail closed on unknown names).
$baselinePortDir = Join-Path $vcpkgRoot "ports/ffmpeg"
if (!(Test-Path -LiteralPath $baselinePortDir -PathType Container)) { throw "vcpkg ffmpeg port missing at baseline: $baselinePortDir" }
$baselinePortManifestPath = Join-Path $baselinePortDir "vcpkg.json"
if (!(Test-Path -LiteralPath $baselinePortManifestPath -PathType Leaf)) { throw "baseline ffmpeg vcpkg.json missing: $baselinePortManifestPath" }
$baselinePortManifest = Get-Content -Raw -LiteralPath $baselinePortManifestPath | ConvertFrom-Json
$baselineFeatures = @()
if ($null -ne $baselinePortManifest.features) {
    $baselineFeatures = @($baselinePortManifest.features.PSObject.Properties.Name)
}
Write-Host ("baseline ffmpeg port features: " + ($baselineFeatures -join ", "))
if ($baselineFeatures -notcontains "ffmpeg") {
    "available baseline ffmpeg features: $($baselineFeatures -join ', ')" | Out-File -Encoding utf8 (Join-Path $evidenceDir "baseline-ffmpeg-features.txt")
    Get-Content -Raw -LiteralPath $baselinePortManifestPath | Out-File -Encoding utf8 (Join-Path $evidenceDir "baseline-ffmpeg-vcpkg.json")
    throw "baseline ffmpeg port has no 'ffmpeg' executable feature; cannot satisfy the test-only tool requirement without inventing a feature (see baseline-ffmpeg-features.txt)"
}
# Refuse to silently use 9.0.2: the baseline port version must be recorded, never trusted as the build output.
$baselineVersion = $baselinePortManifest.version
if ($null -eq $baselineVersion) { $baselineVersion = $baselinePortManifest.'version-string' }
Write-Host "baseline ffmpeg port version: $baselineVersion (recorded only; build uses pinned commit $ffmpegCommit)"

# Pin the exact FFmpeg source commit without trusting mutable refs.
$workDir = Join-Path $evidenceDir "ffmpeg-source"
if (Test-Path -LiteralPath $workDir) { Remove-Item -Recurse -Force -LiteralPath $workDir }
New-Item -ItemType Directory -Force -Path $workDir | Out-Null
git clone --no-checkout $ffmpegRepo $workDir
if ($LASTEXITCODE -ne 0) { throw "FFmpeg clone failed" }
Push-Location $workDir
try {
    git fetch --depth 1 origin $ffmpegCommit
    if ($LASTEXITCODE -ne 0) { throw "FFmpeg fetch of pinned commit $ffmpegCommit failed" }
    # vcpkg stores its patch files as -text/LF, but FFmpeg Makefiles may be
    # converted by Windows core.autocrlf. Keep patch contexts byte-compatible.
    git -C $workDir config core.autocrlf false
    if ($LASTEXITCODE -ne 0) { throw "FFmpeg checkout line-ending configuration failed" }
    $autoCrlf = ((git -C $workDir config --get core.autocrlf) | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $autoCrlf -ne "false") { throw "FFmpeg checkout must keep core.autocrlf=false" }
    git cat-file -e "$ffmpegCommit^{commit}"
    if ($LASTEXITCODE -ne 0) { throw "pinned FFmpeg commit object is missing after fetch" }
    git checkout --detach $ffmpegCommit --quiet
    if ($LASTEXITCODE -ne 0) { throw "FFmpeg checkout of $ffmpegCommit failed" }
    $actual = ((git rev-parse HEAD) | Out-String).Trim()
    if ($actual -ne $ffmpegCommit) { throw "FFmpeg HEAD $actual != pinned $ffmpegCommit" }
    git log -1 --format="ffmpeg HEAD: %H %ad %s" --date=short
} finally {
    Pop-Location
}

# Derive the source fetch and omit the proof-bound unsupported OpenGL disable.
# Keep every baseline patch in declared order except the hash-pinned 0004, 0007 and 0045
# patches, which receive exact-context replacements for this pinned source.
# Hash-pinned 0047 and 0052 require upstream-supersession proof at their sequential stages.
$baselinePortfile = Join-Path $baselinePortDir "portfile.cmake"
$baselinePortfileHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $baselinePortfile).Hash.ToLowerInvariant()
$gitHelper = Get-Content -Raw -LiteralPath (Join-Path $vcpkgRoot "scripts/cmake/vcpkg_from_git.cmake")
. (Join-Path $scriptDir "derive-ffmpeg-overlay.ps1")
# Validate pristine configure before any sequential patch changes its hash.
# This writes evidence only, never the baseline port or vcpkg checkout.
$openglPortPath = Join-Path $evidenceDir "opengl-portfile.cmake"
$openglJson = & python (Join-Path $scriptDir "ffmpeg-dependency-patch.py") omit-opengl-disable (Join-Path $workDir "configure") $baselinePortfile $openglPortPath
if ($LASTEXITCODE -ne 0) { throw "pinned unsupported OpenGL disable validation failed" }
$portfileTransformation = $openglJson | ConvertFrom-Json
$derived = ConvertTo-PrototypeFfmpegPort (Get-Content -Raw -LiteralPath $openglPortPath) $gitHelper $ffmpegRepo $ffmpegCommit
Remove-Item -LiteralPath $openglPortPath
$adaptations = @(
    [pscustomobject]@{
        baseline = $pins.vcpkg.ffmpeg_dependency_patch.name
        overlay = $pins.vcpkg.ffmpeg_dependency_patch.overlay_name
        pin = $pins.vcpkg.ffmpeg_dependency_patch
        source = "configure"
        command = "adapt"
        label = "dependency"
    },
    [pscustomobject]@{
        baseline = $pins.vcpkg.ffmpeg_lib_naming_patch.name
        overlay = $pins.vcpkg.ffmpeg_lib_naming_patch.overlay_name
        pin = $pins.vcpkg.ffmpeg_lib_naming_patch
        source = "configure"
        command = "adapt-lib-naming"
        label = "library-name"
    },
    [pscustomobject]@{
        baseline = $pins.vcpkg.ffmpeg_bin2c_patch.name
        overlay = $pins.vcpkg.ffmpeg_bin2c_patch.overlay_name
        pin = $pins.vcpkg.ffmpeg_bin2c_patch
        source = "ffbuild/common.mak"
        command = "adapt-bin2c"
        label = "prebuilt-bin2c"
    }
)
if ($adaptations[0].baseline -cne "0004-dependencies.patch" -or $adaptations[0].overlay -cne "0004-dependencies-pinned.patch") { throw "unsupported pinned dependency patch name" }
if ($adaptations[1].baseline -cne "0007-fix-lib-naming.patch" -or $adaptations[1].overlay -cne "0007-fix-lib-naming-pinned.patch") { throw "unsupported pinned library-name patch name" }
if ($adaptations[2].baseline -cne "0045-use-prebuilt-bin2c.patch" -or $adaptations[2].overlay -cne "0045-use-prebuilt-bin2c-pinned.patch") { throw "unsupported pinned prebuilt-bin2c patch name" }
$supersededPatch = $pins.vcpkg.ffmpeg_msvc_utf8_patch
if ($supersededPatch.name -cne "0047-fix-msvc-utf8.patch" -or @($derived.Patches | Where-Object { $_ -ceq $supersededPatch.name }).Count -ne 1) { throw "expected exactly one pinned MSVC UTF-8 patch" }
$swscalePatch = $pins.vcpkg.ffmpeg_unstable_swscale_patch
if ($swscalePatch.name -cne "0052-fix-disable-unstable-swscale-link.patch" -or @($derived.Patches | Where-Object { $_ -ceq $swscalePatch.name }).Count -ne 1) { throw "expected exactly one pinned unstable swscale patch" }
$overlayRaw = $derived.Portfile
foreach ($adaptation in $adaptations) {
    if (@($derived.Patches | Where-Object { $_ -ceq $adaptation.baseline }).Count -ne 1) { throw "baseline FFmpeg patch series must contain exactly one pinned patch: $($adaptation.baseline)" }
    if (@($derived.Patches | Where-Object { $_ -ceq $adaptation.overlay }).Count -ne 0) { throw "baseline FFmpeg patch series already contains adapted patch name: $($adaptation.overlay)" }
    $patchRewrite = Set-PrototypeFfmpegPatch $overlayRaw $adaptation.baseline $adaptation.overlay
    $overlayRaw = $patchRewrite.Portfile
}

# Apply the retained PATCHES sequentially to this clean exact-commit checkout,
# in portfile order (not filename order, nor independently against pristine HEAD).
$patchResults = @()
$patchFailures = @()
$adaptedPatchFiles = @{}
Push-Location $workDir
try {
    foreach ($name in $derived.Patches) {
        $patchPath = Join-Path $baselinePortDir $name
        if ($name -ceq $supersededPatch.name) {
            $supersessionPath = Join-Path $evidenceDir "msvc-utf8-supersession.json"
            & python (Join-Path $scriptDir "ffmpeg-dependency-patch.py") omit-msvc-utf8 (Join-Path $workDir "configure") $patchPath $supersessionPath
            if ($LASTEXITCODE -ne 0) { throw "pinned MSVC UTF-8 upstream-supersession validation failed" }
            $supersession = Get-Content -Raw -LiteralPath $supersessionPath | ConvertFrom-Json
            $overlayRaw = (Set-PrototypeFfmpegPatch $overlayRaw $name '').Portfile
            $patchResults += [ordered]@{ patch = $name; omitted = $true; applied = $false; evidence = $supersession }
            continue
        }
        if ($name -ceq $swscalePatch.name) {
            $supersessionPath = Join-Path $evidenceDir "unstable-swscale-supersession.json"
            & python (Join-Path $scriptDir "ffmpeg-dependency-patch.py") omit-unstable-swscale $workDir $patchPath $supersessionPath
            if ($LASTEXITCODE -ne 0) { throw "pinned unstable swscale upstream-supersession validation failed" }
            $supersession = Get-Content -Raw -LiteralPath $supersessionPath | ConvertFrom-Json
            $overlayRaw = (Set-PrototypeFfmpegPatch $overlayRaw $name '').Portfile
            $patchResults += [ordered]@{ patch = $name; omitted = $true; applied = $false; evidence = $supersession }
            continue
        }
        $adaptation = $adaptations | Where-Object { $_.baseline -ceq $name } | Select-Object -First 1
        if ($null -ne $adaptation) {
            $adaptedPatchPath = Join-Path $evidenceDir $adaptation.overlay
            & python (Join-Path $scriptDir "ffmpeg-dependency-patch.py") $adaptation.command (Join-Path $workDir $adaptation.source) $patchPath $adaptedPatchPath
            if ($LASTEXITCODE -ne 0) { throw "pinned FFmpeg $($adaptation.label)-patch adaptation failed: $name" }
            if (!(Test-Path -LiteralPath $adaptedPatchPath -PathType Leaf)) { throw "adapted patch missing: $adaptedPatchPath" }
            git apply --check --whitespace=nowarn $adaptedPatchPath
            if ($LASTEXITCODE -ne 0) { throw "adapted patch does not apply to pinned FFmpeg after prior patches: $name" }
            git apply --whitespace=nowarn $adaptedPatchPath
            if ($LASTEXITCODE -ne 0) { throw "adapted patch application failed after check: $name" }
            $patchResults += [ordered]@{
                patch = $name
                overlay_patch = $adaptation.overlay
                applies = $true
                adapted = $true
                baseline_sha256 = $adaptation.pin.sha256
                overlay_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $adaptedPatchPath).Hash.ToLowerInvariant()
            }
            $adaptedPatchFiles[$name] = [ordered]@{ path = $adaptedPatchPath; overlay = $adaptation.overlay }
            Write-Host "pinned source-context patch applied: $name -> $($adaptation.overlay)"
            continue
        }
        git apply --check --whitespace=nowarn $patchPath
        if ($LASTEXITCODE -eq 0) {
            git apply --whitespace=nowarn $patchPath
            if ($LASTEXITCODE -ne 0) { throw "patch application failed after check: $name" }
            $patchResults += @{ patch = $name; overlay_patch = $name; applies = $true; adapted = $false }
            Write-Host "patch applied: $name"
        } else {
            $patchResults += @{ patch = $name; overlay_patch = $name; applies = $false; adapted = $false }
            $patchFailures += $name
            Write-Host "patch DOES NOT apply: $name"
            break
        }
    }
} finally {
    Pop-Location
}

$sourceRecord = [ordered]@{
    schema = 1
    ffmpeg = [ordered]@{ repository = $ffmpegRepo; commit = $ffmpegCommit }
    vcpkg = [ordered]@{ root = $vcpkgRoot; baseline = $vcpkgBaseline; head = $vcpkgHead; baseline_port_version = "$baselineVersion" }
    triplet = $effectiveTriplet
    linkage = "dynamic"
    features = "default-only plus test-only ffmpeg executable (no gpl, nonfree, all)"
    portfile_transformation = $portfileTransformation
    patches = $patchResults
}
$sourceRecord | ConvertTo-Json -Depth 6 | Out-File -Encoding utf8 (Join-Path $evidenceDir "ffmpeg-source.json")
if ($patchFailures.Count -gt 0) {
    throw ("vcpkg FFmpeg baseline patch series does not apply cleanly to {0} ({1} failing: {2}); refusing fallback to 9.0.2. Adapt the port patch series for the pinned commit and re-test the build." -f $ffmpegCommit, $patchFailures.Count, ($patchFailures -join ", "))
}
if ($patchResults.Count -eq 0) { throw "baseline ffmpeg port has no patches to validate; refusing to proceed without MSVC patch/config evidence" }

# --- Test-only overlay derived from the pinned baseline port ---
# Overlay invokes vcpkg_from_git(REF db8537d8cc7962bd3d07254d6ceff21d888817d4) against https://github.com/FFmpeg/FFmpeg.git.
# Copy the exact baseline port, then use the guarded source/patch/OpenGL
# derivation while retaining unrelated baseline configuration. The overlay is
# written under target/ (never into the vcpkg checkout or release paths).
$overlayRoot = Join-Path $evidenceDir "overlay-ports"
$overlayPortDir = Join-Path $overlayRoot "ffmpeg"
if (Test-Path -LiteralPath $overlayRoot) { Remove-Item -Recurse -Force -LiteralPath $overlayRoot }
New-Item -ItemType Directory -Force -Path $overlayPortDir | Out-Null
Copy-Item -Recurse -Force -Path (Join-Path $baselinePortDir "*") -Destination $overlayPortDir
# Retain omitted 0047 and 0052 only in the exact baseline source-bundle section.
Remove-Item -LiteralPath (Join-Path $overlayPortDir $supersededPatch.name)
Remove-Item -LiteralPath (Join-Path $overlayPortDir $swscalePatch.name)
foreach ($name in $adaptedPatchFiles.Keys) {
    $adaptedPatch = $adaptedPatchFiles[$name]
    if (!(Test-Path -LiteralPath $adaptedPatch.path -PathType Leaf)) { throw "adapted patch missing before overlay copy: $name" }
    Copy-Item -Force -LiteralPath $adaptedPatch.path -Destination (Join-Path $overlayPortDir $adaptedPatch.overlay)
}
$overlayPortfile = Join-Path $overlayPortDir "portfile.cmake"
if (!(Test-Path -LiteralPath $overlayPortfile -PathType Leaf)) { throw "overlay portfile missing after copy: $overlayPortfile" }
# Do not append a newline: source-bundle proof preserves unrelated port text.
[System.IO.File]::WriteAllText($overlayPortfile, $overlayRaw, [System.Text.UTF8Encoding]::new($false))
$overlayPortfileHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $overlayPortfile).Hash.ToLowerInvariant()
# Overlay port version must not claim 9.0.2: mark it as the pinned commit.
$overlayManifestPath = Join-Path $overlayPortDir "vcpkg.json"
if (Test-Path -LiteralPath $overlayManifestPath -PathType Leaf) {
    $overlayManifestRaw = Get-Content -Raw -LiteralPath $overlayManifestPath
    # Replace the baseline version with a pinned version-string; keep features/dependencies.
    $overlayManifest = $overlayManifestRaw | ConvertFrom-Json
    # Remove `version` if present and set `version-string` to the pinned commit marker.
    if ($null -ne $overlayManifest.version) { $overlayManifest.PSObject.Properties.Remove("version") }
    $overlayManifest | Add-Member -NotePropertyName "version-string" -NotePropertyValue "pinned-$ffmpegCommit" -Force
    $overlayManifest | ConvertTo-Json -Depth 8 | Out-File -Encoding utf8 $overlayManifestPath
    $overlayManifestCheck = Get-Content -Raw -LiteralPath $overlayManifestPath
    if ($overlayManifestCheck -match "9\.0\.2") { throw "overlay vcpkg.json still references 9.0.2 after pinning; refusing fallback" }
}
Write-Host "overlay derived from baseline $vcpkgBaseline with REF $ffmpegCommit"
git -C $vcpkgRoot diff --no-index --stat $baselinePortDir $overlayPortDir 2>&1 | Select-Object -First 20
$overlayRaw | Out-File -Encoding utf8 (Join-Path $evidenceDir "overlay-portfile.cmake")
(Get-FileHash -Algorithm SHA256 -LiteralPath $overlayPortfile).Hash | Out-File -Encoding utf8 (Join-Path $evidenceDir "overlay-portfile.sha256")
[ordered]@{
    schema = 1
    baseline = [ordered]@{ root = $baselinePortDir; baseline = $vcpkgBaseline; portfile_sha256 = $baselinePortfileHash }
    overlay = [ordered]@{ dir = $overlayPortDir; portfile_sha256 = $overlayPortfileHash; ref = $ffmpegCommit; repo = $ffmpegRepo }
    portfile_transformation = $portfileTransformation
    patches = $patchResults
} | ConvertTo-Json -Depth 6 | Out-File -Encoding utf8 (Join-Path $evidenceDir "overlay.json")

# Retain deterministic pristine source + exact baseline port/patches + recipe.
$sourceBundle = Join-Path $evidenceDir "source-bundle"
& python (Join-Path $scriptDir "source-bundle.py") create $workDir $vcpkgRoot $overlayPortDir $sourceBundle
if ($LASTEXITCODE -ne 0) { throw "prototype FFmpeg source bundle generation/verification failed" }
$sourceBundleManifest = Join-Path $sourceBundle "MANIFEST.json"
$sourceBundleRecord = [ordered]@{
    sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $sourceBundleManifest).Hash.ToLowerInvariant()
    bytes = (Get-Item -LiteralPath $sourceBundleManifest).Length
}

# --- Install/build FFmpeg from the exact pinned commit via the overlay ---
New-Item -ItemType Directory -Force -Path $installedDir | Out-Null
$installLog = Join-Path $evidenceDir "vcpkg-install.log"
if (Test-Path -LiteralPath $installLog) { Remove-Item -LiteralPath $installLog -Force }
Write-Host "running vcpkg install (overlay + custom dynamic triplet, no binary caching)..."
& $vcpkgExe install --triplet $effectiveTriplet --overlay-ports $overlayRoot --overlay-triplets $tripletsDir --x-manifest-root $scriptDir --x-install-root $installedDir --no-binarycaching 2>&1 | Tee-Object -FilePath $installLog
if ($LASTEXITCODE -ne 0) {
    throw "vcpkg install of pinned FFmpeg $ffmpegCommit failed (see vcpkg-install.log); refusing fallback to 9.0.2"
}
if (!(Test-Path -LiteralPath $installLog -PathType Leaf)) { throw "vcpkg install log missing: $installLog" }
$logRaw = Get-Content -Raw -LiteralPath $installLog
if ($logRaw -match "9\.0\.2" -and $logRaw -notmatch [regex]::Escape($ffmpegCommit)) {
    throw "vcpkg install log references 9.0.2 without the pinned commit; refusing to accept a fallback build"
}

# Installed closure must exist under the isolated install root + custom triplet.
$tripletBin = Join-Path $installedDir "$effectiveTriplet/bin"
if (!(Test-Path -LiteralPath $tripletBin -PathType Container)) {
    Get-ChildItem -Recurse -Directory -LiteralPath $installedDir -ErrorAction SilentlyContinue | Select-Object -First 20 FullName | ForEach-Object { Write-Host $_.FullName }
    throw "FFmpeg DLL closure missing: $tripletBin not found under isolated VCPKG_ROOT/installed"
}
$dlls = @(Get-ChildItem -LiteralPath $tripletBin -Filter "*.dll" -File -ErrorAction SilentlyContinue)
if ($dlls.Count -eq 0) { throw "FFmpeg DLL closure is empty in $tripletBin" }
Write-Host "installed $($dlls.Count) FFmpeg DLLs in $tripletBin"
$dllRecords = @()
foreach ($d in ($dlls | Sort-Object Name)) {
    $dllRecords += [ordered]@{
        file = $d.Name
        bytes = $d.Length
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $d.FullName).Hash.ToLowerInvariant()
    }
}

# Locate the test-only ffmpeg executable (tool feature) for deterministic smoke-video generation.
$toolCandidates = @(
    (Join-Path $installedDir "$effectiveTriplet/tools/ffmpeg/bin/ffmpeg.exe"),
    (Join-Path $installedDir "$effectiveTriplet/tools/ffmpeg/ffmpeg.exe"),
    (Join-Path $tripletBin "ffmpeg.exe")
)
$ffmpegExe = $toolCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if (!$ffmpegExe) {
    $found = Get-ChildItem -Recurse -File -LiteralPath $installedDir -Filter "ffmpeg.exe" -ErrorAction SilentlyContinue | Select-Object -First 5
    if ($found) { $found | ForEach-Object { Write-Host $_.FullName } }
    throw "test-only ffmpeg.exe not found under $installedDir (the 'ffmpeg' tool feature must have built it); refusing PATH fallback to unknown ffmpeg"
}
Write-Host "ffmpeg tool: $ffmpegExe"
$ffmpegExe | Out-File -Encoding utf8 (Join-Path $evidenceDir "ffmpeg-tool.path")
if ($env:GITHUB_ENV) { "IRIS_PROTOTYPE_FFMPEG_EXE=$ffmpegExe" | Out-File -Encoding utf8 -Append $env:GITHUB_ENV }

# Record actual build configuration and version from the exact-commit tool.
$buildconfPath = Join-Path $evidenceDir "ffmpeg-buildconf.txt"
$versionPath = Join-Path $evidenceDir "ffmpeg-version.txt"
& $ffmpegExe -hide_banner -buildconf 2>&1 | Out-File -Encoding utf8 $buildconfPath
if ($LASTEXITCODE -ne 0) { throw "ffmpeg -buildconf failed for $ffmpegExe" }
& $ffmpegExe -hide_banner -version 2>&1 | Select-Object -First 5 | Out-File -Encoding utf8 $versionPath
if ($LASTEXITCODE -ne 0) { throw "ffmpeg -version failed for $ffmpegExe" }
$buildconfRaw = Get-Content -Raw -LiteralPath $buildconfPath
$versionRaw = Get-Content -Raw -LiteralPath $versionPath
Write-Host ($versionRaw -split "`n" | Select-Object -First 2 | Out-String).Trim()
if ($versionRaw -match "9\.0\.2" -and $buildconfRaw -notmatch [regex]::Escape($ffmpegCommit)) {
    Write-Host "WARNING: tool reports 9.0.2 without pinned-commit evidence; failing closed"
    throw "built ffmpeg tool does not evidence the pinned commit $ffmpegCommit (see ffmpeg-buildconf.txt); refusing fallback"
}
# LGPL-only config evidence: fail closed on gpl/nonfree in the actual buildconf.
if ($buildconfRaw -match "(?i)--enable-gpl") { throw "built FFmpeg enables gpl (LGPL-only required); see ffmpeg-buildconf.txt" }
if ($buildconfRaw -match "(?i)--enable-nonfree") { throw "built FFmpeg enables nonfree (LGPL-only required); see ffmpeg-buildconf.txt" }
$buildconfRaw | Select-Object -First 1 | ForEach-Object { Write-Host $_ }

# Full build evidence (no licensing conclusion).
[ordered]@{
    schema = 1
    ffmpeg = [ordered]@{ repository = $ffmpegRepo; commit = $ffmpegCommit; actual = $ffmpegCommit }
    vcpkg = [ordered]@{ root = $vcpkgRoot; baseline = $vcpkgBaseline; head = $vcpkgHead }
    triplet = $effectiveTriplet
    linkage = "dynamic"
    features = "default-only plus test-only ffmpeg executable (no gpl, nonfree, all)"
    tools = [ordered]@{ git = $gitVer; cmake = $cmakeVer; ninja = $ninjaVer; python = $pythonVer; cl = $clOut }
    overlay = [ordered]@{ dir = $overlayPortDir; portfile_sha256 = $overlayPortfileHash; baseline_portfile_sha256 = $baselinePortfileHash }
    install = [ordered]@{ root = $installedDir; bin = $tripletBin; dlls = @($dllRecords); tool = "$ffmpegExe" }
    portfile_transformation = $portfileTransformation
    source_bundle_manifest = $sourceBundleRecord
    buildconf = ($buildconfRaw -split "`n" | Select-Object -First 5)
    note = "Prototype-only exact-commit FFmpeg build evidence. No licensing conclusion."
} | ConvertTo-Json -Depth 8 | Out-File -Encoding utf8 (Join-Path $evidenceDir "ffmpeg-build.json")

Write-Host "exact-commit FFmpeg build passed: $ffmpegCommit ($($dlls.Count) DLLs, tool $ffmpegExe)"
