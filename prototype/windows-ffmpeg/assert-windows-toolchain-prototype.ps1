# Prototype-only pinned Windows Server 2022 toolchain assertion.
# Test-only; never a release payload. Fails closed on any drift.
$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "../..")).Path
$pinsPath = Join-Path $scriptDir "pins.json"
$evidenceDir = Join-Path $repoRoot "target/prototype-windows-ffmpeg"
New-Item -ItemType Directory -Force -Path $evidenceDir | Out-Null

$pins = Get-Content -Raw -LiteralPath $pinsPath | ConvertFrom-Json
$vcpkgBaseline = $pins.vcpkg.baseline
if ($vcpkgBaseline -ne "b8dfe957ec8b5c0ef5c4b31eea594b8486af744f") { throw "pins.json vcpkg baseline drifted: $vcpkgBaseline" }

# --- OS: must be Windows Server 2022 (Build 20348), x64 ---
$os = Get-CimInstance Win32_OperatingSystem
$caption = $os.Caption
$build = $os.BuildNumber
$arch = $os.OSArchitecture
Write-Host "OS: $caption ($build, $arch)"
"OS: $caption ($build, $arch)" | Out-File -Encoding utf8 (Join-Path $evidenceDir "toolchain-os.txt")
if ($caption -notmatch "Windows Server 2022") { throw "prototype requires Windows Server 2022 (windows-2022 runner); got '$caption'" }
if ($build -ne "20348") { throw "prototype requires Server 2022 build 20348; got '$build'" }
if ($arch -notmatch "64-bit") { throw "prototype requires 64-bit OS; got '$arch'" }

# --- Required tools must exist; versions are logged for evidence ---
foreach ($tool in @("git", "cmake", "ninja", "python")) {
    if (!(Get-Command $tool -ErrorAction SilentlyContinue)) { throw "required tool is missing: $tool" }
}
Write-Host "--- tool versions ---"
$gitVer = (git --version) 2>&1 | Out-String
$cmakeVer = (cmake --version 2>&1 | Select-Object -First 1) | Out-String
$ninjaVer = (ninja --version) 2>&1 | Out-String
$pythonVer = (python --version) 2>&1 | Out-String
Write-Host $gitVer.Trim()
Write-Host $cmakeVer.Trim()
Write-Host $ninjaVer.Trim()
Write-Host $pythonVer.Trim()

# --- MSVC: cl.exe must be on PATH via the Developer prompt step ---
$cl = Get-Command "cl.exe" -ErrorAction SilentlyContinue
if (!$cl) {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
        Write-Host "--- vswhere instances ---"
        & $vswhere -latest -property installationPath,installationVersion,catalog_productDisplayVersion
    }
    throw "cl.exe is not on PATH; the 'Set up MSVC developer environment (x64)' step must run first"
}
Write-Host ("cl: " + $cl.Source)
$clOut = (& $cl.Source 2>&1 | Select-Object -First 3 | Out-String)
Write-Host $clOut.Trim()
if ($clOut -notmatch "Microsoft \(R\) C/C\+\+ Optimizing Compiler") { throw "unexpected cl.exe output; refusing to proceed" }

# VS instance + SDK for evidence (fail closed when no instance).
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (!(Test-Path -LiteralPath $vswhere -PathType Leaf)) { throw "vswhere.exe missing; VS2022 is required on windows-2022" }
$vsPath = ((& $vswhere -latest -property installationPath | Select-Object -First 1) | Out-String).Trim()
$vsVer = ((& $vswhere -latest -property installationVersion | Select-Object -First 1) | Out-String).Trim()
Write-Host "VS instance: $vsPath ($vsVer)"
if ([string]::IsNullOrWhiteSpace($vsPath)) { throw "vswhere found no VS instance" }
if ($vsVer -notmatch "^17\.") { throw "prototype requires VS2022 (17.x); got '$vsVer'" }
$sdkNote = [Environment]::GetEnvironmentVariable("WindowsSDKVersion")
if ($sdkNote) { Write-Host "WindowsSDKVersion: $sdkNote" } else { Write-Host "WindowsSDKVersion: (not set; using toolchain default, logged for evidence)" }
$vcTools = [Environment]::GetEnvironmentVariable("VCToolsVersion")
if ($vcTools) { Write-Host "VCToolsVersion: $vcTools" }

# --- dumpbin + ISCC presence is checked here for early evidence (install steps own them) ---
$dumpbin = Get-Command "dumpbin.exe" -ErrorAction SilentlyContinue
if ($dumpbin) { Write-Host ("dumpbin: " + $dumpbin.Source) } else { Write-Host "dumpbin: (not on PATH yet; inventory step resolves via VS layout)" }

# --- VCPKG_ROOT must point at the pinned baseline checkout ---
if ([string]::IsNullOrWhiteSpace($env:VCPKG_ROOT)) { throw "VCPKG_ROOT is not set; workflow must supply the pinned vcpkg checkout" }
if (!(Test-Path -LiteralPath $env:VCPKG_ROOT -PathType Container)) { throw "VCPKG_ROOT missing: $env:VCPKG_ROOT" }
$vcpkgRoot = (Resolve-Path $env:VCPKG_ROOT).Path
Write-Host "VCPKG_ROOT: $vcpkgRoot"
$vcpkgHead = ((git -C $vcpkgRoot rev-parse HEAD) | Out-String).Trim()
Write-Host "vcpkg HEAD: $vcpkgHead"
if ($vcpkgHead -ne $vcpkgBaseline) { throw "VCPKG_ROOT HEAD $vcpkgHead != pinned baseline $vcpkgBaseline; refusing to build" }
git -C $vcpkgRoot cat-file -e "$vcpkgBaseline^{commit}"
if ($LASTEXITCODE -ne 0) { throw "pinned vcpkg baseline object missing in VCPKG_ROOT" }
$vcpkgExe = Join-Path $vcpkgRoot "vcpkg.exe"
if (!(Test-Path -LiteralPath $vcpkgExe -PathType Leaf)) { throw "vcpkg.exe missing: $vcpkgExe (bootstrap step must run first)" }
& $vcpkgExe --version
if ($LASTEXITCODE -ne 0) { throw "vcpkg --version failed" }

# --- Triplet env must select the prototype dynamic triplet ---
foreach ($name in @("VCPKG_TARGET_TRIPLET", "VCPKG_DEFAULT_TRIPLET")) {
    $value = [Environment]::GetEnvironmentVariable($name)
    if ([string]::IsNullOrWhiteSpace($value)) { throw "$name is not set; workflow must set the prototype dynamic triplet" }
    if ($value -ne "x64-windows-prototype-dynamic") { throw "$name must be x64-windows-prototype-dynamic (dynamic), got '$value'" }
}
if ([Environment]::GetEnvironmentVariable("VCPKGRS_DYNAMIC") -ne "1") { throw "VCPKGRS_DYNAMIC must be 1 for dynamic FFmpeg linkage" }
if ([string]::IsNullOrWhiteSpace($env:VCPKG_INSTALLED_DIR)) { throw "VCPKG_INSTALLED_DIR is not set; workflow must supply the isolated install root" }
Write-Host "VCPKG_INSTALLED_DIR: $env:VCPKG_INSTALLED_DIR"
Write-Host "prototype toolchain assertion passed (Server 2022 + VS2022 + pinned vcpkg baseline)"

[ordered]@{
    schema = 1
    os = [ordered]@{ caption = $caption; build = $build; arch = $arch }
    tools = [ordered]@{ git = $gitVer.Trim(); cmake = $cmakeVer.Trim(); ninja = $ninjaVer.Trim(); python = $pythonVer.Trim(); cl = $clOut.Trim() }
    vs = [ordered]@{ path = $vsPath; version = $vsVer; sdk = $sdkNote; vcTools = $vcTools }
    vcpkg = [ordered]@{ root = $vcpkgRoot; baseline = $vcpkgBaseline; head = $vcpkgHead }
    triplet = "x64-windows-prototype-dynamic"
} | ConvertTo-Json -Depth 6 | Out-File -Encoding utf8 (Join-Path $evidenceDir "toolchain.json")
