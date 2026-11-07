# Pure source rewrite, shared with the bounded regression check. No downloads/builds.
function ConvertTo-PrototypeFfmpegPort {
    param([string]$Portfile, [string]$GitHelper, [string]$Repository, [string]$Commit)

    if ($Repository -ne 'https://github.com/FFmpeg/FFmpeg.git' -or $Commit -notmatch '^[0-9a-f]{40}$') {
        throw 'invalid pinned FFmpeg repository/commit'
    }
    # Inspect the actual pinned helper, not the similarly named GitHub helper.
    $signature = [regex]::Match($GitHelper, '(?s)cmake_parse_arguments\(\s*PARSE_ARGV\s+0\s+"?arg"?\s+"[^"]*"\s+"(?<single>[^"]*)"\s+"(?<multi>[^"]*)"\s*\)')
    if (!$signature.Success) { throw 'unrecognized vcpkg_from_git argument parser' }
    $single = $signature.Groups['single'].Value.Split(';')
    foreach ($required in @('OUT_SOURCE_PATH', 'URL', 'REF', 'HEAD_REF')) {
        if ($single -cnotcontains $required) { throw "vcpkg_from_git does not accept $required" }
    }
    if ($signature.Groups['multi'].Value.Split(';') -cnotcontains 'PATCHES') { throw 'vcpkg_from_git does not accept PATCHES' }

    $fetches = [regex]::Matches($Portfile, '(?ms)^vcpkg_from_(?:github|git)\(\s*\n.*?^\)')
    if ($fetches.Count -ne 1) { throw 'expected exactly one baseline source fetch' }
    $fetch = $fetches[0]
    # Only this known baseline GitHub shape is supported. In particular, REPO
    # may be unquoted; it is not a valid parameter of vcpkg_from_git.
    $shape = '(?s)^vcpkg_from_github\(\s+OUT_SOURCE_PATH\s+SOURCE_PATH\s+REPO\s+"?ffmpeg/ffmpeg"?\s+REF\s+[^\r\n]+\r?\n\s*SHA512\s+[0-9a-fA-F]{128}\s+HEAD_REF\s+master\s+PATCHES\s+(?<patches>.*?)\s*\)$'
    $source = [regex]::Match($fetch.Value, $shape)
    if (!$source.Success) { throw 'unsupported pinned baseline FFmpeg fetch parameters' }
    $patches = @()
    foreach ($line in ($source.Groups['patches'].Value -split '\r?\n')) {
        $entry = $line.Trim()
        if ([string]::IsNullOrWhiteSpace($entry)) { continue }
        # Preserve comments in the generated portfile, but identify the patch
        # path before an optional CMake `#` comment for ordered applicability.
        if ($entry -match '^"(?<quoted>[^"#]+\.patch)"(?:\s+#.*)?$') {
            $patch = $Matches['quoted']
        } elseif ($entry -match '^(?<plain>[A-Za-z0-9_-][A-Za-z0-9_./-]*\.patch)(?:\s+#.*)?$') {
            $patch = $Matches['plain']
        } else {
            throw "unsupported baseline PATCHES entry: $line"
        }
        if ($patch -match '(^|/)\.\.(/|$)') { throw "unsafe baseline patch path: $patch" }
        $patches += $patch
    }
    if ($patches.Count -eq 0) { throw 'baseline PATCHES is empty' }

    $replacement = $fetch.Value -creplace '^vcpkg_from_github', 'vcpkg_from_git'
    $replacement = $replacement -creplace '(?m)^\s*REPO\s+[^\r\n]+', "    URL $Repository"
    $replacement = $replacement -creplace '(?m)^\s*REF\s+[^\r\n]+', "    REF $Commit"
    $replacement = $replacement -creplace '(?m)^\s*SHA512\s+[^\r\n]+\r?\n', ''
    # Validate the complete generated call, not just the presence of a REF.
    $valid = '(?s)^vcpkg_from_git\(\s+OUT_SOURCE_PATH\s+SOURCE_PATH\s+URL\s+' + [regex]::Escape($Repository) + '\s+REF\s+' + $Commit + '\s+HEAD_REF\s+master\s+PATCHES\s+' + [regex]::Escape($source.Groups['patches'].Value) + '\s*\)$'
    if ($replacement -cnotmatch $valid) { throw 'invalid generated vcpkg_from_git parameters' }
    # Everything outside the fetch, and the PATCHES text, stays byte-for-byte.
    return [pscustomobject]@{
        Portfile = $Portfile.Substring(0, $fetch.Index) + $replacement + $Portfile.Substring($fetch.Index + $fetch.Length)
        Patches = $patches
    }
}

# Replace exactly one PATCHES entry, or remove it after supersession validation.
# This is separate from source-ref rewriting so callers can prove the
# baseline order and the authorized replacements.
function Set-PrototypeFfmpegPatch {
    param([string]$Portfile, [string]$BaselinePatch, [string]$OverlayPatch)

    $names = @($BaselinePatch)
    if ($OverlayPatch -ne '') { $names += $OverlayPatch }
    foreach ($name in $names) {
        if ($name -notmatch '^[A-Za-z0-9_-][A-Za-z0-9_.-]*\.patch$') {
            throw 'invalid dependency patch filename'
        }
    }
    if ($BaselinePatch -ceq $OverlayPatch) { throw 'dependency patch replacement must use a distinct filename' }
    $fetches = [regex]::Matches($Portfile, '(?ms)^vcpkg_from_(?:github|git)\(.*?\bPATCHES\s*\n.*?^\)')
    if ($fetches.Count -ne 1) { throw 'expected exactly one FFmpeg PATCHES block for replacement' }
    # Match before CRLF or LF without consuming the line ending; comments must
    # not accidentally make CRLF entries behave differently from bare entries.
    $pattern = '(?m)^(?<indent>[ \t]*)(?<quote>")?' + [regex]::Escape($BaselinePatch) + '(?(quote)")(?<suffix>[ \t]*(?:#[^\r\n]*)?)(?=\r?$)'
    $matches = [regex]::Matches($fetches[0].Value, $pattern)
    if ($matches.Count -ne 1) { throw "expected exactly one $BaselinePatch entry in PATCHES" }
    $overlayPattern = '(?m)^[ \t]*(?<quote>")?' + [regex]::Escape($OverlayPatch) + '(?(quote)")(?:[ \t]*(?:#[^\r\n]*)?)(?=\r?$)'
    if ($OverlayPatch -ne "" -and [regex]::IsMatch($fetches[0].Value, $overlayPattern)) { throw "PATCHES already contains $OverlayPatch" }
    $evaluator = [System.Text.RegularExpressions.MatchEvaluator]{
        param($match)
        return $match.Groups['indent'].Value + $match.Groups['quote'].Value + $OverlayPatch + $match.Groups['quote'].Value + $match.Groups['suffix'].Value
    }
    $lineRegex = [regex]::new($pattern, [System.Text.RegularExpressions.RegexOptions]::Multiline)
    if ($OverlayPatch -eq '') {
        # Remove the complete entry/comment and its newline, preserving all others.
        $lineRegex = [regex]::new($pattern + '\r?\n')
        $rewrittenFetch = $lineRegex.Replace($fetches[0].Value, '', 1)
    } else {
        $rewrittenFetch = $lineRegex.Replace($fetches[0].Value, $evaluator, 1)
    }
    if ($rewrittenFetch -ceq $fetches[0].Value) { throw 'dependency PATCHES entry was not replaced' }
    $rewritten = $Portfile.Substring(0, $fetches[0].Index) + $rewrittenFetch + $Portfile.Substring($fetches[0].Index + $fetches[0].Length)
    return [pscustomobject]@{ Portfile = $rewritten }
}
