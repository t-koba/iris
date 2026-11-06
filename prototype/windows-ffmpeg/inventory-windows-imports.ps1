param(
    [string]$AppDir = ""
)
# Prototype-only PE import inventory. Test-only; never a release payload.
# Recursively inventories Iris.exe and every staged DLL/EXE via dumpbin,
# distinguishes the Windows OS DLL allowlist from the VC redistributable
# requirements (unknown CRTs are never treated as OS), and fails on
# unknown/missing imports. The output manifest covers all packaged files.
$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "../..")).Path
if ([string]::IsNullOrWhiteSpace($AppDir)) {
    $AppDir = Join-Path $repoRoot "target/prototype-windows-app"
}
$osAllowPath = Join-Path $scriptDir "windows-system-dll-allowlist.txt"
$vcAllowPath = Join-Path $scriptDir "windows-vc-redist-allowlist.txt"
if (!(Test-Path -LiteralPath $AppDir -PathType Container)) { throw "prototype app dir missing: $AppDir" }
if (!(Test-Path -LiteralPath $osAllowPath -PathType Leaf)) { throw "OS allowlist missing: $osAllowPath" }
if (!(Test-Path -LiteralPath $vcAllowPath -PathType Leaf)) { throw "VC redist allowlist missing: $vcAllowPath" }

$dumpbin = Get-Command "dumpbin.exe" -ErrorAction SilentlyContinue
if (!$dumpbin) {
    $vsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path -LiteralPath $vsWhere -PathType Leaf) {
        $vsRoot = ((& $vsWhere -latest -property installationPath | Select-Object -First 1 | Out-String)).Trim()
        if (![string]::IsNullOrWhiteSpace($vsRoot)) {
            $found = Get-ChildItem -Recurse -File -LiteralPath (Join-Path $vsRoot "VC/Tools/MSVC") -Filter "dumpbin.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
            if ($found) { $dumpbin = $found }
        }
    }
}
if (!$dumpbin) { throw "dumpbin.exe is not available; run inside the VS Developer prompt on Windows (MSVC setup step)" }
$dumpbinPath = if ($dumpbin.Source) { $dumpbin.Source } else { $dumpbin.FullName }
Write-Host "dumpbin: $dumpbinPath"

function Read-Allowlist($path) {
    return @(Get-Content -LiteralPath $path | ForEach-Object {
        $line = ($_ -split '#', 2)[0].Trim().ToLowerInvariant()
        $line
    } | Where-Object { $_ -ne "" })
}
$osAllow = Read-Allowlist $osAllowPath
$vcAllow = Read-Allowlist $vcAllowPath
if ($osAllow.Count -eq 0) { throw "OS allowlist is empty" }
if ($vcAllow.Count -eq 0) { throw "VC redist allowlist is empty" }

function Test-Pattern($lower, $patterns) {
    foreach ($pattern in $patterns) {
        if ($pattern -eq $lower) { return $true }
        if ($pattern.Contains("*") -and ($lower -like $pattern)) { return $true }
    }
    return $false
}
function Test-AllowedOS($name) { return (Test-Pattern $name.ToLowerInvariant() $osAllow) }
function Test-AllowedVC($name) { return (Test-Pattern $name.ToLowerInvariant() $vcAllow) }
function Test-LooksLikeCrt($lower) {
    return ($lower -like "msvc*.dll" -or $lower -like "vcruntime*.dll" -or $lower -like "msvcp*.dll" -or $lower -like "ucrt*.dll" -or $lower -like "api-ms-win-crt-*.dll" -or $lower -like "concrt*.dll" -or $lower -like "vcomp*.dll")
}

function Get-Imports($binary) {
    $out = & $dumpbinPath /DEPENDENTS $binary 2>&1
    if ($LASTEXITCODE -ne 0) { throw "dumpbin failed for $binary" }
    $imports = @()
    foreach ($line in $out) {
        if ($line -match '^\s+([A-Za-z0-9_][A-Za-z0-9_\-\.]*\.dll)\s*$') {
            $imports += $Matches[1]
        }
    }
    return @($imports | Sort-Object -Unique)
}

# Recursively discover every staged PE (EXE/DLL) under the app dir.
$peFiles = @(Get-ChildItem -Recurse -File -LiteralPath $AppDir -ErrorAction Stop | Where-Object { $_.Extension -match '(?i)^\.(dll|exe)$' } | Sort-Object FullName)
if ($peFiles.Count -eq 0) { throw "no PE files found under $AppDir" }
$iris = Join-Path $AppDir "Iris.exe"
if (!(Test-Path -LiteralPath $iris -PathType Leaf)) { throw "Iris.exe missing in $AppDir" }
Write-Host "discovered $($peFiles.Count) PE files under $AppDir"
$staged = @{}
foreach ($f in $peFiles) { $staged[$f.Name.ToLowerInvariant()] = $f.FullName }

$queue = @($peFiles | ForEach-Object { $_.FullName })
$seen = @{}
$records = @()
$problems = @()
while ($queue.Count -gt 0) {
    $current = $queue[0]
    if ($queue.Count -gt 1) { $queue = $queue[1..($queue.Count - 1)] } else { $queue = @() }
    $key = $current.ToLowerInvariant()
    if ($seen.ContainsKey($key)) { continue }
    $seen[$key] = $true
    $imports = Get-Imports $current
    $leaf = Split-Path -Leaf $current
    $entry = [ordered]@{
        file = $leaf
        path = $current
        staged = $true
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $current).Hash.ToLowerInvariant()
        bytes = (Get-Item -LiteralPath $current).Length
        imports = @($imports)
        resolutions = @()
    }
    $resolutions = @()
    foreach ($import in $imports) {
        $lower = $import.ToLowerInvariant()
        if ($staged.ContainsKey($lower)) {
            $resolutions += [ordered]@{ dll = $import; kind = "staged" }
            $target = $staged[$lower]
            $tkey = $target.ToLowerInvariant()
            if (!$seen.ContainsKey($tkey)) { $queue += $target }
            continue
        }
        if ($lower -eq "iris.exe") {
            $resolutions += [ordered]@{ dll = $import; kind = "staged" }
            continue
        }
        if (Test-AllowedOS $lower) {
            $resolutions += [ordered]@{ dll = $import; kind = "os" }
            continue
        }
        if (Test-AllowedVC $lower) {
            $resolutions += [ordered]@{ dll = $import; kind = "vc-redist" }
            continue
        }
        if (Test-LooksLikeCrt $lower) {
            $problems += ("{0} imports CRT {1} (not staged, not in VC redist allowlist; never treated as OS)" -f $leaf, $import)
        } else {
            $problems += ("{0} imports {1} (neither staged nor allowlisted as OS/VC-redist)" -f $leaf, $import)
        }
        $resolutions += [ordered]@{ dll = $import; kind = "unknown" }
    }
    $entry.resolutions = @($resolutions)
    $records += $entry
}

# Cover ALL packaged files (not just PEs) with hashes, excluding the manifest
# being written to avoid a self-hash cycle.
$allPackaged = @(Get-ChildItem -Recurse -File -LiteralPath $AppDir -ErrorAction Stop | Where-Object { $_.Name -ne "IMPORT-CLOSURE.json" } | Sort-Object FullName | ForEach-Object {
    $rel = $_.FullName.Substring($AppDir.Length).TrimStart('\', '/')
    [ordered]@{
        file = $rel
        bytes = $_.Length
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant()
    }
})
Write-Host "packaged files (excluding IMPORT-CLOSURE.json): $($allPackaged.Count)"

$report = [ordered]@{
    schema = 1
    app_dir = $AppDir
    dumpbin = $dumpbinPath
    files = $records
    packaged_files = @($allPackaged)
    packaged_count = $allPackaged.Count
    os_allowlist = @($osAllow)
    vc_allowlist = @($vcAllow)
    problems = @($problems)
}
$reportPath = Join-Path $AppDir "IMPORT-CLOSURE.json"
$report | ConvertTo-Json -Depth 8 | Out-File -Encoding utf8 $reportPath
Write-Host "inventoried $($records.Count) binaries ($($allPackaged.Count) packaged files); report: $reportPath"
if ($problems.Count -gt 0) {
    foreach ($p in $problems) { Write-Host "IMPORT PROBLEM: $p" }
    throw ("import closure has {0} unknown/missing imports; refusing to pass" -f $problems.Count)
}
Write-Host "import closure is clean: all imports are staged, OS-allowlisted, or VC-redist-allowlisted"
