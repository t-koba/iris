"""Bounded source guards; run the PowerShell fault-injection tests if available."""
from pathlib import Path
import shutil
import subprocess
import unittest

ROOT = Path(__file__).resolve().parent
SMOKE = (ROOT / "smoke-windows-installed-previews.ps1").read_text()
LIFECYCLE = (ROOT / "installed-smoke-lifecycle.ps1").read_text()
BUILD = (ROOT / "build-windows-ffmpeg-prototype.ps1").read_text()
STAGE = (ROOT / "stage-windows-prototype-app.ps1").read_text()


class SmokeEvidenceTests(unittest.TestCase):
    def test_success_is_only_after_lifecycle_returns(self):
        self.assertEqual(SMOKE.count('$evidence.result = "pass"'), 1)
        self.assertIn('result = "fail"', SMOKE)
        reset = SMOKE.index("$evidence | ConvertTo-Json")
        self.assertLess(reset, SMOKE.index('foreach ($p in'))
        lifecycle = SMOKE.index("Invoke-PrototypeInstalledSmoke -InstallDir")
        success = SMOKE.index('$evidence.result = "pass"')
        self.assertLess(lifecycle, SMOKE.index("} -Smoke {", lifecycle))
        self.assertLess(SMOKE.index("} -Uninstall {", lifecycle), success)
        self.assertIn('    }\n    # No success evidence', SMOKE[:success])
        self.assertIn('} catch {\n    $evidence.result = "fail"', SMOKE[success:])
        self.assertIn('} finally {\n    $evidence | ConvertTo-Json', SMOKE[success:])
        self.assertNotIn('result = "pass"', LIFECYCLE)

    def test_all_cargo_builds_use_root_installed_and_custom_triplet(self):
        for source in (BUILD, STAGE, SMOKE):
            self.assertIn('$env:VCPKGRS_TRIPLET = $effectiveTriplet', source)
            self.assertIn('VCPKG_INSTALLED_DIR must equal VCPKG_ROOT/installed', source)
        self.assertIn('Join-Path $vcpkgRoot "installed"', BUILD)
        self.assertIn('Join-Path (Resolve-Path $env:VCPKG_ROOT).Path "installed"', STAGE)
        self.assertIn('Join-Path $vcpkgRoot "installed"', SMOKE)

    def test_cleanup_does_not_hide_uninstall_failure(self):
        self.assertIn("} finally {", LIFECYCLE)
        self.assertIn("& $Uninstall\n            $Evidence.uninstall = 'pass'", LIFECYCLE)
        self.assertIn("$Evidence.uninstall_error =", LIFECYCLE)
        cleanup = LIFECYCLE.index("Remove-Item -LiteralPath $InstallDir -Recurse -Force")
        self.assertLess(LIFECYCLE.index("$Evidence.uninstall_error ="), cleanup)
        self.assertLess(cleanup, LIFECYCLE.index("$Evidence.cleanup = 'pass'"))
        self.assertIn("if (Test-Path -LiteralPath $InstallDir) { throw", LIFECYCLE[cleanup:])
        self.assertIn("if ($failed) { throw", LIFECYCLE)
        self.assertNotIn("SilentlyContinue", LIFECYCLE)
        self.assertIn("$message.Substring(0, [Math]::Min($message.Length, $ErrorLimit))", LIFECYCLE)
        for process in ("result", "uninstall"):
            self.assertIn(f"if (${process}.ExitCode -ne 0)", SMOKE)
        self.assertIn('if ($LASTEXITCODE -ne 0) { throw "prototype preview smoke failed', SMOKE)

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell unavailable; no runtime execution claimed")
    def test_fault_injection(self):
        subprocess.run(
            ["pwsh", "-NoProfile", "-File", str(ROOT / "test-installed-smoke-lifecycle.ps1")],
            check=True, timeout=30,
        )


if __name__ == "__main__":
    unittest.main()
