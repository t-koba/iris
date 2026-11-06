# Bounded offline regression: exercises the real rewrite, no native build.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'derive-ffmpeg-overlay.ps1')
$commit = 'db8537d8cc7962bd3d07254d6ceff21d888817d4'
$repo = 'https://git.ffmpeg.org/ffmpeg.git'
$helper = 'cmake_parse_arguments(PARSE_ARGV 0 "arg" "" "OUT_SOURCE_PATH;URL;REF;FETCH_REF;HEAD_REF" "PATCHES")'
# Representative baseline shape, not a claim to be the exact upstream port.
$port = @'
set(KEEP_CONFIG unchanged)
vcpkg_from_github(
    OUT_SOURCE_PATH SOURCE_PATH
    REPO ffmpeg/ffmpeg
    REF "n${VERSION}"
    SHA512 HASH
    HEAD_REF master
    PATCHES
        z-first.patch # retained CMake comment
        "a-second.patch"
)
set(KEEP_REF "REF unchanged")
'@ -replace 'HASH', ('a' * 128)
# PowerShell may preserve checkout line endings in the here-string. Normalize
# first so the CRLF case is exactly CRLF, never CRCRLF.
$portLf = $port.Replace("`r`n", "`n").Replace("`r", "")
$portCrLf = $portLf.Replace("`n", "`r`n")
$quotedPort = $portLf.Replace('REPO ffmpeg/ffmpeg', 'REPO "ffmpeg/ffmpeg"')
foreach ($inputPort in @($portLf, $quotedPort, $portCrLf)) {
    $result = ConvertTo-PrototypeFfmpegPort $inputPort $helper $repo $commit
    if (($result.Patches -join ',') -cne 'z-first.patch,a-second.patch') { throw 'patch order changed' }
    $expected = $inputPort.Replace('vcpkg_from_github(', 'vcpkg_from_git(')
    $expected = $expected.Replace('REPO ffmpeg/ffmpeg', "URL $repo").Replace('REPO "ffmpeg/ffmpeg"', "URL $repo")
    $expected = $expected.Replace('REF "n${VERSION}"', "REF $commit")
    $expected = $expected -replace '(?m)^    SHA512 [a-f0-9]+\r?\n', ''
    if ($result.Portfile -cne $expected) { throw 'rewrite modified unrelated config/patch text or used invalid fetch parameters' }
}
$patchPort = @'
vcpkg_from_git(
    OUT_SOURCE_PATH SOURCE_PATH
    URL https://git.ffmpeg.org/ffmpeg.git
    REF db8537d8cc7962bd3d07254d6ceff21d888817d4
    HEAD_REF master
    PATCHES
        first.patch
        "0004-dependencies.patch" # retained explanation
        last.patch
)
'@
$replacedPatch = Set-PrototypeFfmpegDependencyPatch $patchPort '0004-dependencies.patch' '0004-dependencies-pinned.patch'
if ($replacedPatch.Portfile -notmatch '(?m)^        "0004-dependencies-pinned\.patch" # retained explanation$') { throw 'adapted PATCHES entry/order/comment was not preserved' }
foreach ($badPatch in @($patchPort.Replace('0004-dependencies.patch', 'other.patch'), $patchPort.Replace('last.patch', '0004-dependencies-pinned.patch'))) {
    $rejected = $false
    try { Set-PrototypeFfmpegDependencyPatch $badPatch '0004-dependencies.patch' '0004-dependencies-pinned.patch' | Out-Null } catch { $rejected = $true }
    if (!$rejected) { throw 'accepted missing/duplicate dependency patch entry' }
}

foreach ($bad in @($port.Replace('REPO ffmpeg/ffmpeg', 'URL bogus'), $port.Replace('z-first.patch', '../escape.patch'), $port.Replace('HEAD_REF master', 'UNKNOWN argument'), ($port + "`nvcpkg_from_git(`n)`n"))) {
    $rejected = $false
    try { ConvertTo-PrototypeFfmpegPort $bad $helper $repo $commit | Out-Null } catch { $rejected = $true }
    if (!$rejected) { throw 'accepted unsupported baseline fetch' }
}
$rejected = $false
try { ConvertTo-PrototypeFfmpegPort $port ($helper.Replace(';URL;', ';REPO;')) $repo $commit | Out-Null } catch { $rejected = $true }
if (!$rejected) { throw 'accepted incompatible helper signature' }

# Guard discovery and sequential patch application in the caller without builds.
$build = Get-Content -Raw (Join-Path $PSScriptRoot 'build-windows-ffmpeg-prototype.ps1')
$stage = Get-Content -Raw (Join-Path $PSScriptRoot 'stage-windows-prototype-app.ps1')
$smoke = Get-Content -Raw (Join-Path $PSScriptRoot 'smoke-windows-installed-previews.ps1')
foreach ($script in @($build, $stage, $smoke)) {
    if ($script -notmatch '\$env:VCPKGRS_TRIPLET = \$effectiveTriplet' -or $script -notmatch 'VCPKG_INSTALLED_DIR must equal VCPKG_ROOT/installed') { throw 'Rust discovery root/triplet regression' }
}
if ($build -notmatch '\$installedDir = Join-Path \$vcpkgRoot "installed"' -or $stage -notmatch '\$installedDir = Join-Path \(Resolve-Path \$env:VCPKG_ROOT\)\.Path "installed"' -or $smoke -notmatch '\$installedDirEnv = Join-Path \$vcpkgRoot "installed"') { throw 'Rust discovery must use VCPKG_ROOT/installed' }
if ($build -notmatch '(?s)foreach \(\$name in \$derived.Patches\).*ffmpeg-dependency-patch.py.*adapt.*git apply --check --whitespace=nowarn \$adaptedPatchPath') { throw 'pinned dependency patch adaptation/application is not ordered and guarded' }
if ($build -notmatch 'git apply --check --whitespace=nowarn \$patchPath' -or $build -notmatch 'Set-PrototypeFfmpegDependencyPatch') { throw 'baseline patch sequence or fail-closed overlay replacement is missing' }
Write-Host 'prototype overlay/discovery regressions passed (no native build)'
