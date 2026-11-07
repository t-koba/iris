# Bounded offline regression: exercises the real rewrite, no native build.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'derive-ffmpeg-overlay.ps1')
$commit = 'db8537d8cc7962bd3d07254d6ceff21d888817d4'
$repo = 'https://github.com/FFmpeg/FFmpeg.git'
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
    URL https://github.com/FFmpeg/FFmpeg.git
    REF db8537d8cc7962bd3d07254d6ceff21d888817d4
    HEAD_REF master
    PATCHES
        first.patch
        "0004-dependencies.patch" # retained explanation
        0007-fix-lib-naming.patch
        0045-use-prebuilt-bin2c.patch
        last.patch
)
'@
# Exercise both line endings explicitly, independently of the checkout. A CRLF
# duplicate without a comment used to evade the overlay-entry regex's `$` anchor.
$patchPortLf = $patchPort.Replace("`r`n", "`n").Replace("`r", "")
foreach ($ending in @("`n", "`r`n")) {
    foreach ($entry in @('"0004-dependencies.patch"', '0004-dependencies.patch')) {
        foreach ($comment in @('', ' # retained explanation')) {
            $inputPatch = $patchPortLf.Replace('"0004-dependencies.patch" # retained explanation', $entry + $comment).Replace("`n", $ending)
            $replacedPatch = Set-PrototypeFfmpegPatch $inputPatch '0004-dependencies.patch' '0004-dependencies-pinned.patch'
            $expectedPatch = $inputPatch.Replace('0004-dependencies.patch', '0004-dependencies-pinned.patch')
            $replacedNaming = Set-PrototypeFfmpegPatch $replacedPatch.Portfile '0007-fix-lib-naming.patch' '0007-fix-lib-naming-pinned.patch'
            $expectedPatch = $expectedPatch.Replace('0007-fix-lib-naming.patch', '0007-fix-lib-naming-pinned.patch')
            $replacedBin2c = Set-PrototypeFfmpegPatch $replacedNaming.Portfile '0045-use-prebuilt-bin2c.patch' '0045-use-prebuilt-bin2c-pinned.patch'
            $expectedPatch = $expectedPatch.Replace('0045-use-prebuilt-bin2c.patch', '0045-use-prebuilt-bin2c-pinned.patch')
            if ($replacedBin2c.Portfile -cne $expectedPatch) { throw 'adapted PATCHES entry/order/comment/line endings were not preserved' }
            $badPatches = [ordered]@{
                missing = $inputPatch.Replace('0004-dependencies.patch', 'other.patch')
                duplicateBaseline = $inputPatch.Replace('last.patch', $entry + $comment)
                duplicateQuotedOverlay = $inputPatch.Replace('last.patch', '"0004-dependencies-pinned.patch"')
                duplicateUnquotedOverlay = $inputPatch.Replace('last.patch', '0004-dependencies-pinned.patch')
            }
            foreach ($case in $badPatches.Keys) {
                $rejected = $false
                try { Set-PrototypeFfmpegPatch $badPatches[$case] '0004-dependencies.patch' '0004-dependencies-pinned.patch' | Out-Null } catch { $rejected = $true }
                if (!$rejected) { throw "accepted $case dependency patch entry (CRLF=$($ending.Length -eq 2), entry=$entry, comment=$comment)" }
            }
        }
    }
}

# Omissions use the same exact-entry matcher: quotes/comments/CRLF do not
# change the proof of one entry, nor may they change surrounding patch bytes.
foreach ($patchName in @('0047-fix-msvc-utf8.patch', '0052-fix-disable-unstable-swscale-link.patch')) {
    foreach ($ending in @("`n", "`r`n")) {
        foreach ($entry in @($patchName, ('"' + $patchName + '"'))) {
            foreach ($comment in @('', ' # superseded upstream')) {
                $line = '        ' + $entry + $comment + $ending
                $inputPatch = $patchPortLf.Replace("        last.patch`n", ('        ' + $entry + $comment + "`n        last.patch`n")).Replace("`n", $ending)
                $omitted = Set-PrototypeFfmpegPatch $inputPatch $patchName ''
                if ($omitted.Portfile -cne $inputPatch.Replace($line, '')) { throw 'omission changed other patch bytes/order' }
                foreach ($badPatch in @($omitted.Portfile, $inputPatch.Replace('last.patch', $entry))) {
                    $rejected = $false
                    try { Set-PrototypeFfmpegPatch $badPatch $patchName '' | Out-Null } catch { $rejected = $true }
                    if (!$rejected) { throw 'accepted missing/duplicate superseded patch' }
                }
            }
        }
    }
}

foreach ($bad in @($port.Replace('REPO ffmpeg/ffmpeg', 'URL bogus'), $port.Replace('z-first.patch', '../escape.patch'), $port.Replace('HEAD_REF master', 'UNKNOWN argument'), ($port + "`nvcpkg_from_git(`n)`n"))) {
    $rejected = $false
    try { ConvertTo-PrototypeFfmpegPort $bad $helper $repo $commit | Out-Null } catch { $rejected = $true }
    if (!$rejected) { throw 'accepted unsupported baseline fetch' }
}
foreach ($badRepo in @('https://git.ffmpeg.org/ffmpeg.git', 'https://github.com/other/FFmpeg.git')) {
    $rejected = $false
    try { ConvertTo-PrototypeFfmpegPort $port $helper $badRepo $commit | Out-Null } catch { $rejected = $true }
    if (!$rejected) { throw 'accepted unpinned source repository' }
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
if ($build -notmatch 'git apply --check --whitespace=nowarn \$patchPath' -or $build -notmatch 'Set-PrototypeFfmpegPatch') { throw 'baseline patch sequence or fail-closed overlay replacement is missing' }
Write-Host 'prototype overlay/discovery regressions passed (no native build)'

foreach ($omissionCase in @(
    @{ variable = 'supersededPatch'; command = 'omit-msvc-utf8' },
    @{ variable = 'swscalePatch'; command = 'omit-unstable-swscale' }
)) {
    $omission = $build.IndexOf('if ($name -ceq $' + $omissionCase.variable + '.name)')
    $validate = $build.IndexOf($omissionCase.command, $omission)
    $remove = $build.IndexOf("Set-PrototypeFfmpegPatch `$overlayRaw `$name ''", $validate)
    $skip = $build.IndexOf('continue', $remove)
    $apply = $build.IndexOf('git apply --check', $skip)
    if ($omission -lt 0 -or $validate -le $omission -or $remove -le $validate -or $skip -le $remove -or $apply -le $skip) { throw 'supersession must validate, remove and skip before patch application' }
    if (!$build.Contains('Remove-Item -LiteralPath (Join-Path $overlayPortDir $' + $omissionCase.variable + '.name)')) { throw 'superseded patch must not be copied into the overlay' }
}

# Execute the real staging filter with synthetic records, without staging/builds.
$filter = [regex]::Match($stage, '(?s)\$failed = @\(\$gate.patches \| Where-Object \{.*?\n\}\)')
if (!$filter.Success) { throw 'staging patch filter missing' }
$filterCode = [scriptblock]::Create($filter.Value)
$pins = Get-Content -Raw (Join-Path $PSScriptRoot 'pins.json') | ConvertFrom-Json
$validOmission = @{
    patch = $pins.vcpkg.ffmpeg_msvc_utf8_patch.name; omitted = $true; applied = $false
    evidence = @{ baseline_sha256 = $pins.vcpkg.ffmpeg_msvc_utf8_patch.sha256; ffmpeg_commit = $pins.ffmpeg.commit; source_sha256 = ('a' * 64) }
}
$validSwscaleOmission = @{
    patch = $pins.vcpkg.ffmpeg_unstable_swscale_patch.name; omitted = $true; applied = $false
    evidence = @{
        baseline_sha256 = $pins.vcpkg.ffmpeg_unstable_swscale_patch.sha256
        ffmpeg_commit = $pins.ffmpeg.commit; vcpkg_baseline = $pins.vcpkg.baseline
        source_sha256 = $pins.vcpkg.ffmpeg_unstable_swscale_patch.source_sha256
    }
}
foreach ($record in @(@{ applies = $true }, $validOmission, $validSwscaleOmission)) {
    $gate = @{ patches = @($record) }
    . $filterCode
    if ($failed.Count -ne 0) { throw 'staging rejected applied patch or verified omission' }
}
foreach ($valid in @($validOmission, $validSwscaleOmission)) {
    foreach ($field in @('patch', 'omitted', 'applied', 'evidence')) {
        $bad = $valid.Clone()
        $bad[$field] = $null
        $gate = @{ patches = @($bad) }
        . $filterCode
        if ($failed.Count -ne 1) { throw "staging accepted invalid omission: $field" }
    }
}
foreach ($field in @('baseline_sha256', 'ffmpeg_commit', 'vcpkg_baseline', 'source_sha256')) {
    $bad = $validSwscaleOmission.Clone()
    $bad.evidence = $validSwscaleOmission.evidence.Clone()
    $bad.evidence[$field] = $null
    $gate = @{ patches = @($bad) }
    . $filterCode
    if ($failed.Count -ne 1) { throw "staging accepted incomplete swscale evidence: $field" }
}
