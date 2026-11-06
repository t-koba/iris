# Prototype evidence lifecycle. Callers must supply a fresh, uniquely owned path.
# Scriptblocks allow bounded fault-injection tests without native installs.
function Invoke-PrototypeInstalledSmoke {
    param(
        [string]$InstallDir,
        [System.Collections.IDictionary]$Evidence,
        [scriptblock]$Install,
        [scriptblock]$Smoke,
        [scriptblock]$Uninstall,
        [ValidateScript({ $_ -gt 0 })][int]$ErrorLimit = 1024
    )
    $failed = $false
    $phase = 'install'
    try {
        $Evidence.install = 'fail'
        & $Install
        $Evidence.install = 'pass'
        $phase = 'smoke'
        $Evidence.smoke = 'fail'
        & $Smoke
        $Evidence.smoke = 'pass'
    } catch {
        $failed = $true
        $message = $_.Exception.Message
        $Evidence["${phase}_error"] = $message.Substring(0, [Math]::Min($message.Length, $ErrorLimit))
    } finally {
        # A failed/partial install can still have an uninstaller. Failure here
        # must neither mask the original error nor skip directory cleanup.
        try {
            $Evidence.uninstall = 'fail'
            & $Uninstall
            $Evidence.uninstall = 'pass'
        } catch {
            $failed = $true
            $message = $_.Exception.Message
            $Evidence.uninstall_error = $message.Substring(0, [Math]::Min($message.Length, $ErrorLimit))
        }
        try {
            $Evidence.cleanup = 'fail'
            if (Test-Path -LiteralPath $InstallDir) {
                Remove-Item -LiteralPath $InstallDir -Recurse -Force
            }
            if (Test-Path -LiteralPath $InstallDir) { throw 'unique install directory still exists after cleanup' }
            $Evidence.cleanup = 'pass'
        } catch {
            $failed = $true
            $message = $_.Exception.Message
            $Evidence.cleanup_error = $message.Substring(0, [Math]::Min($message.Length, $ErrorLimit))
        }
    }
    if ($failed) { throw 'prototype install/smoke/uninstall/cleanup failed; see smoke-result.json phase evidence' }
}
