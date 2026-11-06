# Prototype-only clean-install smoke: video, generated PDF, synthetic RAW.
# Test-only; never a release payload. Fails closed. Builds the detached smoke
# with the same prototype env and runs it only after a fresh install.
# Diagnostic character limit is configurable; default keeps each phase bounded.
param([ValidateScript({ $_ -gt 0 })][int]$ErrorLimit = 1024)
$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "../..")).Path
$appDir = Join-Path $repoRoot "target/prototype-windows-app"
$ffmpegDir = Join-Path $repoRoot "target/prototype-windows-ffmpeg"
$smokeManifest = Join-Path $scriptDir "preview-smoke/Cargo.toml"
$issPath = Join-Path $scriptDir "windows-prototype-installer.iss"
$toolPathFile = Join-Path $ffmpegDir "ffmpeg-tool.path"

$outDir = Join-Path $repoRoot "target/prototype-windows-installer"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$resultPath = Join-Path $outDir "smoke-result.json"
$evidence = [ordered]@{
    schema = 2
    result = "fail"
    install = "not_attempted"
    smoke = "not_attempted"
    uninstall = "not_attempted"
    cleanup = "not_attempted"
}
# Invalidate previous success before any preflight/build/install can fail.
$evidence | ConvertTo-Json -Depth 5 | Out-File -Encoding utf8 $resultPath
try {
    . (Join-Path $scriptDir 'installed-smoke-lifecycle.ps1')
    foreach ($p in @($appDir, $smokeManifest, $issPath)) {
        if (!(Test-Path -LiteralPath $p)) { throw "prototype input missing: $p" }
    }
    if (!(Test-Path -LiteralPath (Join-Path $appDir "Iris.exe") -PathType Leaf)) { throw "staged Iris.exe missing" }
    if (!(Test-Path -LiteralPath (Join-Path $appDir "pdfium.dll") -PathType Leaf)) { throw "staged pdfium.dll missing" }
    if (@(Get-ChildItem -LiteralPath $appDir -Filter "*.dll" -File).Count -lt 2) { throw "staged FFmpeg DLL closure missing" }
    if (!(Test-Path -LiteralPath (Join-Path $appDir "IMPORT-CLOSURE.json") -PathType Leaf)) {
        throw "IMPORT-CLOSURE.json missing; run inventory-windows-imports.ps1 first"
    }
    if (!(Test-Path -LiteralPath $toolPathFile -PathType Leaf)) { throw "ffmpeg tool path missing: $toolPathFile; run build-windows-ffmpeg-prototype.ps1 first" }
    $prototypeFfmpegExe = (Get-Content -Raw -LiteralPath $toolPathFile).Trim()
    if (!(Test-Path -LiteralPath $prototypeFfmpegExe -PathType Leaf)) { throw "prototype ffmpeg.exe missing: $prototypeFfmpegExe" }
    Write-Host "prototype ffmpeg tool for smoke generation: $prototypeFfmpegExe"
    & $prototypeFfmpegExe -hide_banner -version 2>&1 | Select-Object -First 2

    # Detached smoke must request the product default preview features (video, pdf,
    # raw-full). Refuse a minimal/desynchronized smoke manifest.
    $smokeManifestRaw = Get-Content -Raw -LiteralPath $smokeManifest
    if ($smokeManifestRaw -notmatch "raw-full") { throw "preview-smoke manifest must enable default raw-full (not minimal)" }
    if ($smokeManifestRaw -notmatch "test-fixtures") { throw "preview-smoke manifest must enable test-fixtures" }

    $iscc = Get-Command "ISCC.exe" -ErrorAction SilentlyContinue
    if ($iscc) { $isccPath = $iscc.Source } else {
        $isccPath = Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6/ISCC.exe"
        if (!(Test-Path -LiteralPath $isccPath -PathType Leaf)) { throw "Inno Setup 6 is required: install it or add ISCC.exe to PATH" }
    }

    $metadataJson = cargo metadata --no-deps --format-version 1 --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed" }
    $metadata = $metadataJson | ConvertFrom-Json
    $appPackage = @($metadata.packages | Where-Object { $_.name -eq "iris-ui-shell" })
    if ($appPackage.Count -ne 1) { throw "expected one iris-ui-shell package" }
    $appVersion = $appPackage[0].version
    if ($appVersion -notmatch '^(\d+\.\d+\.\d+)(?:[-+].*)?$') { throw "invalid Iris package version: $appVersion" }

    $setupExe = Join-Path $outDir "Iris-prototype-setup.exe"
    if (Test-Path -LiteralPath $setupExe) { Remove-Item -LiteralPath $setupExe -Force }
    & $isccPath "/DAppVersion=$appVersion" "/DSourceDir=$appDir" "/O$outDir" $issPath
    if ($LASTEXITCODE -ne 0) { throw "prototype Inno Setup compilation failed" }
    if (!(Test-Path -LiteralPath $setupExe -PathType Leaf)) { throw "prototype installer was not produced" }
    $setupBytes = (Get-Item -LiteralPath $setupExe).Length
    $setupSha = (Get-FileHash -Algorithm SHA256 -LiteralPath $setupExe).Hash.ToLowerInvariant()
    Write-Host "prototype installer: $setupBytes bytes, sha256 $setupSha"
    $evidence.installer = [ordered]@{ file = "Iris-prototype-setup.exe"; bytes = $setupBytes; sha256 = $setupSha }
    $evidence.video_tool = $prototypeFfmpegExe

    # Same prototype env for the detached smoke build (dynamic linkage, isolated install).
    $installedDirEnv = $env:VCPKG_INSTALLED_DIR
    if ([string]::IsNullOrWhiteSpace($installedDirEnv)) { $installedDirEnv = Join-Path $repoRoot "target/prototype-vcpkg-installed" }
    $effectiveTriplet = $env:VCPKG_TARGET_TRIPLET
    if ([string]::IsNullOrWhiteSpace($effectiveTriplet)) { $effectiveTriplet = $env:IRIS_PROTOTYPE_TRIPLET }
    if ([string]::IsNullOrWhiteSpace($effectiveTriplet)) { $effectiveTriplet = "x64-windows-prototype-dynamic" }
    $env:VCPKGRS_DYNAMIC = "1"
    $env:VCPKG_TARGET_TRIPLET = $effectiveTriplet
    $env:VCPKG_DEFAULT_TRIPLET = $effectiveTriplet
    $env:VCPKG_INSTALLED_DIR = $installedDirEnv
    $env:IRIS_PROTOTYPE_FFMPEG_EXE = $prototypeFfmpegExe
    Write-Host "smoke build env: VCPKGRS_DYNAMIC=1 triplet=$effectiveTriplet installed=$installedDirEnv"
    cargo build --locked --release --manifest-path $smokeManifest
    if ($LASTEXITCODE -ne 0) { throw "prototype preview-smoke build failed" }
    $smokeExe = Join-Path $scriptDir "preview-smoke/target/release/iris-windows-prototype-preview-smoke.exe"
    if (!(Test-Path -LiteralPath $smokeExe -PathType Leaf)) {
        $smokeExe = Join-Path $repoRoot "target/release/iris-windows-prototype-preview-smoke.exe"
    }
    if (!(Test-Path -LiteralPath $smokeExe -PathType Leaf)) { throw "prototype preview-smoke executable missing after build" }

    # Fresh install only: unique temp dir that must not pre-exist.
    $baseTemp = $env:RUNNER_TEMP
    if ([string]::IsNullOrWhiteSpace($baseTemp)) { $baseTemp = [System.IO.Path]::GetTempPath() }
    $installDir = Join-Path $baseTemp ("Iris-prototype-" + [Guid]::NewGuid().ToString("N"))
    if (Test-Path -LiteralPath $installDir) { throw "install dir already exists: $installDir" }
    Write-Host "fresh install dir: $installDir"
    $evidence.install_dir = $installDir
    Invoke-PrototypeInstalledSmoke -InstallDir $installDir -Evidence $evidence -ErrorLimit $ErrorLimit -Install {
        $result = Start-Process -FilePath $setupExe -Wait -PassThru -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', "/DIR=`"$installDir`"")
        if ($result.ExitCode -ne 0) { throw "prototype installer failed: $($result.ExitCode)" }
    } -Smoke {
        foreach ($required in @("Iris.exe", "pdfium.dll")) {
            if (!(Test-Path -LiteralPath (Join-Path $installDir $required) -PathType Leaf)) {
                throw "installed $required missing in $installDir"
            }
        }
        # Installed bytes must match staged bytes (both Iris and PDFium DLLs).
        foreach ($name in @("Iris.exe", "pdfium.dll")) {
            $installedHash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $installDir $name)).Hash
            $stagedHash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $appDir $name)).Hash
            if ($installedHash -ne $stagedHash) { throw "installed $name differs from staged $name" }
        }
        $stagedDlls = @(Get-ChildItem -LiteralPath $appDir -Filter "*.dll" -File | Sort-Object Name)
        $installedDlls = @(Get-ChildItem -LiteralPath $installDir -Filter "*.dll" -File | Sort-Object Name)
        if ($installedDlls.Count -lt $stagedDlls.Count) { throw "installed DLL count $($installedDlls.Count) < staged $($stagedDlls.Count)" }
        Write-Host "installed DLLs: $($installedDlls.Count) (staged $($stagedDlls.Count))"

        # Run the smoke from inside the installed dir so the pdfium.dll beside the
        # executable is the one pdfium-render binds to. PATH is prefixed so the
        # installed FFmpeg DLL closure resolves before any system copy.
        $installedSmoke = Join-Path $installDir "iris-prototype-preview-smoke.exe"
        Copy-Item -LiteralPath $smokeExe -Destination $installedSmoke -Force
        $workDir = Join-Path $installDir "smoke-work"
        New-Item -ItemType Directory -Force -Path $workDir | Out-Null
        $prevPath = $env:PATH
        $env:PATH = "$installDir;$prevPath"
        try {
            Push-Location $installDir
            try {
                & $installedSmoke --app-dir $installDir --work-dir $workDir
                if ($LASTEXITCODE -ne 0) { throw "prototype preview smoke failed (video, pdf, raw)" }
            } finally {
                Pop-Location
            }
        } finally {
            $env:PATH = $prevPath
        }
    } -Uninstall {
        $uninstaller = Join-Path $installDir "unins000.exe"
        if (Test-Path -LiteralPath $uninstaller -PathType Leaf) {
            $uninstall = Start-Process -FilePath $uninstaller -Wait -PassThru -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART')
            if ($uninstall.ExitCode -ne 0) { throw "prototype uninstaller failed: $($uninstall.ExitCode)" }
        } else {
            throw "prototype uninstaller missing: $uninstaller"
        }
        # Verify uninstall cleanup: no staged payload may remain.
        $leftovers = @()
        foreach ($name in @("Iris.exe", "pdfium.dll")) {
            if (Test-Path -LiteralPath (Join-Path $installDir $name)) { $leftovers += $name }
        }
        $remainingDlls = @()
        if (Test-Path -LiteralPath $installDir) {
            $remainingDlls = @(Get-ChildItem -LiteralPath $installDir -Filter "*.dll" -File)
        }
        if ($remainingDlls.Count -gt 0) { $leftovers += @($remainingDlls | ForEach-Object { $_.Name }) }
        if ($leftovers.Count -gt 0) {
            Write-Host ("uninstall leftovers: " + ($leftovers -join ", "))
            throw ("uninstaller left payload behind: {0}" -f ($leftovers -join ", "))
        }
        Write-Host "prototype uninstall cleanup verified (no Iris.exe/pdfium.dll/FFmpeg DLLs remain)"
    }
    # No success evidence until all four lifecycle phases have completed.
    $evidence.result = "pass"
} catch {
    $evidence.result = "fail"
    $message = $_.Exception.Message
    $evidence.error = $message.Substring(0, [Math]::Min($message.Length, $ErrorLimit))
    throw
} finally {
    $evidence | ConvertTo-Json -Depth 5 | Out-File -Encoding utf8 $resultPath
}
Write-Host "prototype clean-install smoke and uninstall/directory cleanup passed (video, pdf, raw)"
