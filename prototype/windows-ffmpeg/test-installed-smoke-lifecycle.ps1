# Offline fault injection only; no Windows installer or native preview claims.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'installed-smoke-lifecycle.ps1')

# Parse the complete production script too, without running its native builds.
$tokens = $null
$parseErrors = $null
[System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'smoke-windows-installed-previews.ps1'),
    [ref]$tokens, [ref]$parseErrors) | Out-Null
if ($parseErrors.Count) { throw ($parseErrors -join "`n") }

# Only cleanup is fault-injected; all other directory operations are real.
function Remove-Item {
    param([string]$LiteralPath, [switch]$Recurse, [switch]$Force)
    if ($script:failCleanup) { throw ('cleanup denied ' + ('x' * 100)) }
    Microsoft.PowerShell.Management\Remove-Item -LiteralPath $LiteralPath -Recurse:$Recurse -Force:$Force
}
$root = Join-Path ([System.IO.Path]::GetTempPath()) ('iris-lifecycle-test-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
try {
    foreach ($case in @('success', 'launch', 'partial', 'smoke', 'uninstall', 'cleanup', 'combined')) {
        $installDir = Join-Path $root $case
        $evidence = [ordered]@{ result = 'fail'; install = 'not_attempted'; smoke = 'not_attempted'; uninstall = 'not_attempted'; cleanup = 'not_attempted' }
        $script:failCleanup = $case -eq 'cleanup'
        $calls = [System.Collections.Generic.List[string]]::new()
        $threw = $false
        try {
            Invoke-PrototypeInstalledSmoke -InstallDir $installDir -Evidence $evidence -ErrorLimit 32 -Install {
                $calls.Add('install')
                if ($case -eq 'launch') { throw 'installer launch failed' }
                New-Item -ItemType Directory -Path $installDir | Out-Null
                Set-Content -LiteralPath (Join-Path $installDir 'payload') -Value 'partial payload'
                if ($case -in @('partial', 'combined')) { throw ('partial install ' + ('x' * 100)) }
            } -Smoke {
                $calls.Add('smoke')
                if ($case -eq 'smoke') { throw ('smoke failed ' + ('x' * 100)) }
            } -Uninstall {
                $calls.Add('uninstall')
                # This also verifies that the helper never publishes early success.
                if ($evidence.result -ne 'fail') { throw 'premature success' }
                if ($case -in @('launch', 'partial')) { throw 'uninstaller missing' }
                if ($case -in @('uninstall', 'combined')) { throw ('uninstall failed ' + ('x' * 100)) }
            }
            # Same final success assignment as the production caller.
            $evidence.result = 'pass'
        } catch { $threw = $true }
        $shouldPass = $case -eq 'success'
        if ($threw -eq $shouldPass -or (($evidence.result -eq 'pass') -ne $shouldPass)) { throw "incorrect final result: $case" }
        $expectSmoke = $case -notin @('launch', 'partial', 'combined')
        if ($calls.Contains('smoke') -ne $expectSmoke -or !$calls.Contains('uninstall')) { throw "incorrect lifecycle order: $case" }
        $expectCalls = if ($expectSmoke) { 'install,smoke,uninstall' } else { 'install,uninstall' }
        if (($calls -join ',') -ne $expectCalls) { throw "incorrect phase sequence: $case" }
        $cleanupFailed = $case -eq 'cleanup'
        if ((Test-Path -LiteralPath $installDir) -ne $cleanupFailed) { throw "incorrect directory cleanup: $case" }
        if (($evidence.cleanup -eq 'pass') -eq $cleanupFailed) { throw "false cleanup evidence: $case" }
        foreach ($phase in @('install', 'smoke', 'uninstall')) {
            $expected = switch ($phase) {
                'install' { if ($case -in @('launch', 'partial', 'combined')) { 'fail' } else { 'pass' } }
                'smoke' { if (!$expectSmoke) { 'not_attempted' } elseif ($case -eq 'smoke') { 'fail' } else { 'pass' } }
                'uninstall' { if ($case -in @('launch', 'partial', 'uninstall', 'combined')) { 'fail' } else { 'pass' } }
            }
            if ($evidence[$phase] -ne $expected) { throw "incorrect $phase evidence: $case" }
            if ($expected -eq 'fail' -and !$evidence.Contains("${phase}_error")) { throw "missing $phase error: $case" }
        }
        # Round-trip the actual evidence shape; diagnostics stay bounded even
        # when primary and uninstall failures happen together.
        $record = $evidence | ConvertTo-Json | ConvertFrom-Json
        foreach ($field in @('install_error', 'smoke_error', 'uninstall_error', 'cleanup_error')) {
            if ($record.$field.Length -gt 32) { throw "unbounded error: $case/$field" }
        }
    }
} finally {
    $script:failCleanup = $false
    Microsoft.PowerShell.Management\Remove-Item -LiteralPath $root -Recurse -Force
}
Write-Host 'installed smoke lifecycle regressions passed (fault injection, no native install)'
