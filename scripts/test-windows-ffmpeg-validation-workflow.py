#!/usr/bin/env python3
"""Offline contracts for the Windows native-validation workflow guard."""

import importlib.util
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]


def load():
    spec = importlib.util.spec_from_file_location(
        "wvwf", ROOT / "scripts/windows-ffmpeg-validation-workflow.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class WorkflowContracts(unittest.TestCase):
    def setUp(self):
        self.mod = load()
        self.text = (ROOT / ".github/workflows/windows-ffmpeg-validation.yml").read_text()

    def test_real_workflow_validates(self):
        checks = self.mod.validate(self.text)
        self.assertEqual(checks["reusable_only"], True)
        self.assertEqual(checks["upload_count"], 1)
        self.assertEqual(checks["upload_literal_count"], 1)
        self.assertTrue(checks["evidence_json_only"])
        self.assertTrue(checks["pe_import_closure"])
        self.assertTrue(checks["staged_only_path"])
        self.assertTrue(checks["stage_binding"])
        self.assertTrue(checks["system_dirs_token_scoped"])
        self.assertTrue(checks["vcpkg_empty_path_guard"])

    def test_all_offline_gates_present(self):
        for gate in self.mod.OFFLINE_GATES:
            self.assertIn(gate, self.text)

    def test_contract_cli_emits_shape(self):
        import json, subprocess
        r = subprocess.run(
            [sys.executable, str(ROOT / "scripts/windows-ffmpeg-validation-workflow.py"), "contract"],
            capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        shape = json.loads(r.stdout)
        self.assertEqual(shape["triggers"], ["workflow_call"])
        self.assertIn("actions/upload-artifact@v4", shape["allowed_actions"])
        for marker in self.mod.ATTESTATION_MARKERS:
            self.assertIn(marker, shape["required_markers"])
        for marker in (self.mod.STAGE_MARKERS + self.mod.PE_IMPORT_MARKERS
                       + self.mod.EVIDENCE_MARKERS + (self.mod.PATH_MARKER,)):
            self.assertIn(marker, shape["required_markers"])

    def test_action_refs_normalizes_yaml_spelling(self):
        mod = self.mod
        cases = (
            ("        uses: actions/upload-artifact@v4", "actions/upload-artifact@v4"),
            ("        uses:  actions/upload-artifact@v4", "actions/upload-artifact@v4"),
            ('        uses: "actions/upload-artifact@v4"', "actions/upload-artifact@v4"),
            ("        uses: 'actions/upload-artifact@v4'", "actions/upload-artifact@v4"),
            ("      - uses: actions/checkout@v4 # pinned", "actions/checkout@v4"),
            ("      - { name: X, uses: actions/upload-artifact@v4, "
             "with: { path: target/debug/Iris.exe } }", "actions/upload-artifact@v4"),
            ("      - uses: ./.github/actions/exfiltrate", "./.github/actions/exfiltrate"),
            ("        uses: /abs/local/action", "/abs/local/action"),
            ("        uses: docker://alpine:3.8", "docker://alpine:3.8"),
        )
        for line, expected in cases:
            with self.subTest(line=line):
                self.assertEqual(mod.action_refs(line), [expected])

    def test_contract_rejects_flow_style_steps(self):
        import json, subprocess
        r = subprocess.run(
            [sys.executable, str(ROOT / "scripts/windows-ffmpeg-validation-workflow.py"), "contract"],
            capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertFalse(json.loads(r.stdout)["flow_steps_allowed"])


class FailClosedContracts(unittest.TestCase):
    def setUp(self):
        self.mod = load()
        self.text = (ROOT / ".github/workflows/windows-ffmpeg-validation.yml").read_text()

    def test_rejects_weakened_attestation_verification(self):
        # Every marker must be executable: removing or weakening any one of
        # them must make the workflow contract fail closed.
        for marker in self.mod.ATTESTATION_MARKERS:
            self.assertIn(marker, self.text)
            with self.subTest(marker=marker):
                mutated = self.text.replace(marker, "<removed>")
                with self.assertRaises(ValueError):
                    self.mod.validate(mutated)

    def test_rejects_attestation_after_extraction(self):
        verify = next(line for line in self.text.splitlines()
                      if "gh attestation verify" in line)
        extract = "          tar -xzf $asset -C $extract\n"
        self.assertEqual(self.text.count(verify + "\n"), 1)
        mutated = self.text.replace(verify + "\n", "", 1).replace(
            extract, extract + verify + "\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_dispatch_trigger(self):
        mutated = self.text.replace("on:\n  workflow_call:\n", "on:\n  workflow_dispatch:\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_release_promotion_marker(self):
        mutated = self.text.replace("Build product with stock locked bindings",
                                    "Build product with IRIS_BUILD_VARIANT: minimal")
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_unsigned_source_marker(self):
        mutated = self.text.replace('if ($pin.version -ne "8.1.3")', 'if ($pin.version -ne "9.1.0")', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_missing_offline_gate(self):
        mutated = self.text.replace(
            "        run: python3 scripts/test-windows-ffmpeg-source.py\n", "")
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_publishing_action(self):
        mutated = self.text.replace(
            "      - name: Assemble bounded evidence manifest",
            "      - uses: softprops/action-gh-release@v2\n      - name: Assemble bounded evidence manifest")
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_native_binary_upload(self):
        mutated = self.text.replace(
            "            build/validation/evidence/windows-validation.json",
            "            target/debug/Iris.exe")
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_missing_cleanup(self):
        mutated = self.text.replace("          rm -rf build/validation/ffmpeg-src", "          true")
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_wrong_source_path_layout(self):
        mutated = self.text.replace(
            'Join-Path $env:IRIS_VALIDATION_SOURCE "ffmpeg-8.1.3"',
            "$env:IRIS_VALIDATION_SOURCE", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_wrong_overlay_port_layout(self):
        mutated = self.text.replace('Join-Path $overlay "ffmpeg"',
                                    'Join-Path $overlay "src"', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_missing_triplet_registration(self):
        mutated = self.text.replace("VCPKG_OVERLAY_TRIPLETS", "VCPKG_UNUSED_TRIPLETS", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_missing_dll_runtime_path(self):
        mutated = self.text.replace(
            'Join-Path $env:VCPKG_INSTALLED_DIR "x64-windows-iris-dynamic/bin"',
            'Join-Path $env:VCPKG_INSTALLED_DIR "missing"', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_gh_release_upload(self):
        mutated = self.text.replace(
            "      - name: Assemble bounded evidence manifest",
            "      - name: Publish\n        run: gh release upload v9.9.9 target/debug/Iris.exe\n"
            "      - name: Assemble bounded evidence manifest", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_write_permission_escalation(self):
        mutated = self.text.replace("  contents: read\n", "  contents: write\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_inline_binary_upload_second_step(self):
        mutated = self.text.replace(
            "      - name: Retain bounded JSON evidence only\n",
            "      - name: Ship native binary\n"
            "        uses: actions/upload-artifact@v4\n"
            "        with:\n"
            "          name: native\n"
            "          path: target/debug/Iris.exe\n"
            "      - name: Retain bounded JSON evidence only\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_anchored_binary_upload_second_step(self):
        mutated = self.text.replace(
            "      - name: Retain bounded JSON evidence only\n",
            "      - name: Ship native binary\n"
            "        uses: &u actions/upload-artifact@v4\n"
            "        with:\n"
            "          name: native\n"
            "          path: target/debug/Iris.exe\n"
            "      - name: Retain bounded JSON evidence only\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_flow_style_binary_upload_second_step(self):
        mutated = self.text.replace(
            "      - name: Retain bounded JSON evidence only\n",
            "      - { name: Ship native binary, uses: actions/upload-artifact@v4, "
            "with: { name: native, path: target/debug/Iris.exe } }\n"
            "      - name: Retain bounded JSON evidence only\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_unlisted_flow_style_action(self):
        mutated = self.text.replace(
            "      - name: Assemble bounded evidence manifest",
            "      - { name: Extra, uses: unrelated/local-action@v1 }\n"
            "      - name: Assemble bounded evidence manifest", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_spaced_binary_upload_second_step(self):
        mutated = self.text.replace(
            "      - name: Retain bounded JSON evidence only\n",
            "      - name: Ship native binary\n"
            "        uses:  actions/upload-artifact@v4\n"
            "        with:\n"
            "          name: native\n"
            "          path: target/debug/Iris.exe\n"
            "      - name: Retain bounded JSON evidence only\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_double_quoted_binary_upload_second_step(self):
        mutated = self.text.replace(
            "      - name: Retain bounded JSON evidence only\n",
            '      - name: Ship native binary\n'
            '        uses: "actions/upload-artifact@v4"\n'
            '        with:\n'
            '          name: native\n'
            '          path: target/debug/Iris.exe\n'
            '      - name: Retain bounded JSON evidence only\n', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_single_quoted_binary_upload_second_step(self):
        mutated = self.text.replace(
            "      - name: Retain bounded JSON evidence only\n",
            "      - name: Ship native binary\n"
            "        uses: 'actions/upload-artifact@v4'\n"
            "        with:\n"
            "          name: native\n"
            "          path: target/debug/Iris.exe\n"
            "      - name: Retain bounded JSON evidence only\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_bare_uses_key_without_action_id(self):
        # Nothing after `uses:` means no reference can be parsed; the line-level
        # allowlist must still reject it rather than silently skipping the step.
        mutated = self.text.replace(
            "      - name: Assemble bounded evidence manifest",
            "      - uses:\n          unrelated/local-action@v1\n"
            "      - name: Assemble bounded evidence manifest", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_anchor_alias_and_tag_keys(self):
        # A `uses` key spelled with an anchor/alias or a tag resolves to the
        # same mapping key at runtime but has no literal `uses` token to scan.
        variants = (
            "      - name: Anchor step\n"
            "        &k uses: actions/checkout@v4\n"
            "      - name: Exfil secrets\n"
            "        *k : ./.github/actions/exfiltrate\n",
            "      - &k uses: actions/checkout@v4\n"
            "      - *k : ./.github/actions/exfiltrate\n",
            "      - &$k uses: actions/checkout@v4\n"
            "      - *$k : ./.github/actions/exfiltrate\n",
            "        &/k uses: actions/checkout@v4\n"
            "        */k : ./.github/actions/exfiltrate\n",
            "        !!str uses: ./.github/actions/exfiltrate\n",
        )
        for variant in variants:
            with self.subTest(variant=variant):
                mutated = self.text.replace(
                    "      - name: Assemble bounded evidence manifest",
                    variant + "      - name: Assemble bounded evidence manifest", 1)
                with self.assertRaises(ValueError):
                    self.mod.validate(mutated)

    def test_rejects_missing_evidence_directory_creation(self):
        # The manifest step must run even when an earlier gate fails before
        # the input gate creates the evidence directory.
        marker = "ev.mkdir(parents=True, exist_ok=True)"
        self.assertIn(marker, self.text)
        mutated = self.text.replace(marker, "<removed>", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_requires_relative_gpg_paths(self):
        # GnuPG on Windows reads a drive-qualified path as a key resource URL.
        # Both the explicit keyring and drive-letter paths must be absent.
        marker = "& gpg --homedir $gpgHome --batch --import $key"
        self.assertIn(marker, self.text)
        mutated = self.text.replace(marker, "& gpg --batch --import $key", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)
        self.assertIn('$gpgDir = "build/validation/gpg"', self.text)
        self.assertIn(
            'New-Item -ItemType Directory -Force -Path $gpgHome | Out-Null',
            self.text)
        self.assertNotIn("--keyring", self.text)
        # PowerShell variable names are case-insensitive: assigning to $home
        # would overwrite the read-only automatic $HOME variable.
        self.assertNotRegex(self.text, r"\$\{?\s*(?:[A-Za-z_][\w-]*:)?\s*home\b")
        for mutation in ('$home = "x"', '$HOME = "x"',
                         '${HOME} = "x"', '$HOME += "x"',
                         'Set-Variable -Name HOME -Value x',
                         'Set-Variable HOME x',
                         'Set-Variable -Name:HOME -Value x',
                         'sv HOME x',
                         '$global:HOME = "x"', '${global:HOME} = "x"',
                         '$script:HOME = "x"', 'set HOME x',
                         'Set-V HOME x', 'Set-Var HOME x',
                         'Set-`Variable HOME x', 'Set-Item variable:HOME x',
                         'New-Variable -Name HOME -Value x',
                         'Get-Variable HOME'):
            mutated = self.text.replace(
                '$gpgHome = "$gpgDir/home"',
                '$gpgHome = "$gpgDir/home"\n          ' + mutation, 1)
            with self.assertRaisesRegex(ValueError, r"read-only PowerShell \$HOME"):
                self.mod.validate(mutated)

    def test_rejects_bare_where_collection_coercion(self):
        # One empty path yields a one-element Collection[psobject], which
        # coerces to the empty element ($false). The explicit count is required.
        marker = '          if (@($manifestRoot, $installRoot, $tripletDir, $env:IRIS_VALIDATION_OVERLAY).Where({ [string]::IsNullOrWhiteSpace($_) }).Count -gt 0) { throw "empty vcpkg path" }\n'
        self.assertIn(marker, self.text)
        mutated = self.text.replace(
            marker,
            marker + '          if (@($manifestRoot).Where({ [string]::IsNullOrWhiteSpace($_) })) { throw "empty vcpkg path" }\n', 1)
        with self.assertRaisesRegex(ValueError, r"match count"):
            self.mod.validate(mutated)

    def test_rejects_commented_out_empty_path_guard(self):
        guard = '          ' + self.mod.VCPKG_EMPTY_PATH_GUARD + '\n'
        self.assertIn(guard, self.text)
        mutated = self.text.replace(
            guard, '          # ' + self.mod.VCPKG_EMPTY_PATH_GUARD + '\n', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_inline_vcpkg_install_root(self):
        # An inline parenthesized option value reached vcpkg as an empty path
        # and failed before any native build. The explicit variable form is
        # mandatory and an additional inline spelling must fail closed.
        mutated = self.text.replace(
            '"--x-install-root=$installRoot"',
            '"--x-install-root=$installRoot"\n'
            '            --x-install-root=(Join-Path $env:IRIS_VALIDATION_VCPKG_ROOT "installed")', 1)
        with self.assertRaisesRegex(ValueError, r"explicit path variable"):
            self.mod.validate(mutated)

    def test_rejects_implicit_missing_input_states(self):
        # A failed preflight must still yield an explicit not_run state rather
        # than an empty stand-in that looks like successful evidence.
        marker = '"status": "not_run"'
        self.assertIn(marker, self.text)
        mutated = self.text.replace(marker, '"status": "recorded"', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_explicit_and_split_yaml_keys(self):
        # Explicit keys/values and key/value split over lines are not used by
        # this workflow; each spelling must fail closed instead of hiding a
        # `uses` step from the line-level scanners.
        variants = (
            "        ? uses\n        : ./.github/actions/exfiltrate\n",
            "      - ? uses\n        : ./.github/actions/exfiltrate\n",
            "        uses\n        : ./.github/actions/exfiltrate\n",
            "        uses: ./.github/actions/exfiltrate\n",
            '        "\\u0075ses": ./.github/actions/exfiltrate\n',
            '        "\\u0075ses": actions/upload-artifact@v4\n',
        )
        for variant in variants:
            with self.subTest(variant=variant):
                mutated = self.text.replace(
                    "      - name: Assemble bounded evidence manifest",
                    "      - name: Exfiltrate secrets\n" + variant
                    + "        with:\n          token: ${{ github.token }}\n"
                    + "      - name: Assemble bounded evidence manifest", 1)
                with self.assertRaises(ValueError):
                    self.mod.validate(mutated)

    def test_rejects_local_and_container_action_refs(self):
        for ref in ("./.github/actions/exfiltrate", "./foo", "/abs/local/action",
                    "docker://alpine:3.8"):
            with self.subTest(ref=ref):
                mutated = self.text.replace(
                    "      - name: Assemble bounded evidence manifest",
                    f"      - uses: {ref}\n"
                    "      - name: Assemble bounded evidence manifest", 1)
                with self.assertRaises(ValueError):
                    self.mod.validate(mutated)

    def test_rejects_unlisted_action(self):
        mutated = self.text.replace(
            "      - name: Assemble bounded evidence manifest",
            "      - uses: unrelated/local-action@v1\n"
            "      - name: Assemble bounded evidence manifest", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_inline_binary_upload_in_sole_step(self):
        mutated = self.text.replace(
            "          path: |\n"
            "            build/validation/evidence/validation-inputs.json\n"
            "            build/validation/evidence/windows-validation.json\n",
            "          path: target/debug/Iris.exe\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)


class StagedRuntimeContracts(unittest.TestCase):
    """The PE evidence and smoke must bind to one isolated staged runtime."""

    def setUp(self):
        self.mod = load()
        self.text = (ROOT / ".github/workflows/windows-ffmpeg-validation.yml").read_text()

    def test_rejects_uninventoried_staged_pdfium_root(self):
        marker = '            (Join-Path $env:IRIS_VALIDATION_STAGE "pdfium.dll")\n'
        self.assertIn(marker, self.text)
        mutated = self.text.replace(marker, '', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_hardcoded_pe_inventory_roots(self):
        old = '            roots = @($rootNames)\n'
        self.assertIn(old, self.text)
        mutated = self.text.replace(
            old, '            roots = @("Iris.exe", "preview_corpus_smoke.exe", "rustc.exe")\n', 1)
        self.assertNotEqual(mutated, self.text)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_missing_pdfium_inventory_assertion(self):
        marker = ('          if (!$visited.ContainsKey("pdfium.dll")) { throw '
                  '"staged pdfium.dll missing from PE inventory" }\n')
        self.assertIn(marker, self.text)
        mutated = self.text.replace(marker, '', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_missing_dumpbin_exit_check(self):
        marker = '          if ($LASTEXITCODE -ne 0) { throw "dumpbin /DEPENDENTS failed for $name" }\n'
        self.assertIn(marker, self.text)
        mutated = self.text.replace(marker, "", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_non_recursive_import_inventory(self):
        for marker in ("          while ($queue.Count -gt 0) {\n",
                       "          $queue.Enqueue($candidate)\n"):
            with self.subTest(marker=marker.strip()):
                self.assertIn(marker, self.text)
                mutated = self.text.replace(marker, "", 1)
                with self.assertRaises(ValueError):
                    self.mod.validate(mutated)

    def test_rejects_unresolved_import_tolerance(self):
        marker = '          if ($unresolved.Count -gt 0) { throw ("unresolved staged PE imports: " + ($unresolved -join ", ")) }\n'
        self.assertIn(marker, self.text)
        mutated = self.text.replace(marker, "          Write-Host \"ignoring unresolved imports\"\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_weakened_system_classification(self):
        marker = '          $isSystem = $import.StartsWith("api-ms-win-") -or $import.StartsWith("ext-ms-win-")\n'
        self.assertIn(marker, self.text)
        mutated = self.text.replace(marker, "          $isSystem = $true\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_hardcoded_system_allowlist(self):
        mutated = self.text.replace(
            '          $systemDirs = @(\n',
            '          $system = @("kernel32.dll")\n          $systemDirs = @(\n', 1)
        self.assertNotEqual(mutated, self.text)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_build_tree_in_system_dirs(self):
        anchor = '            $env:SystemRoot\n          )\n'
        self.assertIn(anchor, self.text)
        for injected in (
            '            (Join-Path $env:VCPKG_INSTALLED_DIR "x64-windows-iris-dynamic/bin")\n',
            '            (Join-Path $env:GITHUB_WORKSPACE "build/validation/overlay")\n',
        ):
            with self.subTest(injected=injected):
                mutated = self.text.replace(anchor, '            $env:SystemRoot\n' + injected + '          )\n', 1)
                self.assertNotEqual(mutated, self.text)
                with self.assertRaises(ValueError):
                    self.mod.validate(mutated)

    def test_rejects_system_dirs_append_or_reassign(self):
        anchor = '          $roots = @(\n'
        self.assertIn(anchor, self.text)
        for injected in (
            '          $systemDirs += (Join-Path $env:VCPKG_INSTALLED_DIR "installed/bin")\n',
            '          $systemDirs = @("C:\\Windows")\n',
            r'          $systemDirs[2] = (Join-Path $env:VCPKG_INSTALLED_DIR "bin")' + '\n',
            r'          $systemDirs[2] += (Join-Path $env:VCPKG_INSTALLED_DIR "bin")' + '\n',
            '          $systemDirs.SetValue((Join-Path $env:VCPKG_INSTALLED_DIR "bin"), 2)\n',
            r'          Set-Variable -Name systemDirs -Value @("C:\Windows")' + '\n',
        ):
            with self.subTest(injected=injected):
                mutated = self.text.replace(anchor, injected + anchor, 1)
                self.assertNotEqual(mutated, self.text)
                with self.assertRaises(ValueError):
                    self.mod.validate(mutated)

    def test_rejects_replacing_system_root_entry(self):
        anchor = '            $env:SystemRoot\n          )\n'
        self.assertIn(anchor, self.text)
        mutated = self.text.replace(anchor, '            $env:GITHUB_WORKSPACE\n          )\n', 1)
        self.assertNotEqual(mutated, self.text)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_missing_system_directory_probe(self):
        marker = '                  if (Test-Path -LiteralPath (Join-Path $sysDir $import) -PathType Leaf) { $isSystem = $true; break }\n'
        self.assertIn(marker, self.text)
        mutated = self.text.replace(marker, '', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_import_resolution_outside_stage(self):
        marker = "          $candidate = Join-Path $stage $import\n"
        self.assertIn(marker, self.text)
        mutated = self.text.replace(
            marker,
            "          $candidate = Join-Path $env:VCPKG_INSTALLED_DIR $import\n", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_forwarder_classification_before_import_binding(self):
        anchor = '          $roots = @(\n'
        self.assertIn(anchor, self.text)
        forwarder = '          $isSystem = $import.StartsWith("api-ms-win-") -or $import.StartsWith("ext-ms-win-")\n'
        mutated = self.text.replace(anchor, forwarder + anchor, 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_bash_release_build(self):
        marker = "        shell: pwsh\n        run: |\n          $ErrorActionPreference = \"Stop\"\n          # Git bash puts its own link.exe ahead of MSVC's"
        self.assertIn(marker, self.text)
        mutated = self.text.replace(marker,
            "        shell: bash\n        run: |\n          set -e\n          # Git bash puts its own link.exe ahead of MSVC's", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_unstaged_path(self):
        mutations = (
            '$env:PATH = "$env:PATH;$stage"',
            '$env:PATH = (@($stage) + $systemPath + "$env:VCPKG_INSTALLED_DIR") '
            '-join [IO.Path]::PathSeparator',
            '$env:PATH = (@($stage) + $systemPath) -join [IO.Path]::PathSeparator\n'
            '          $env:PATH += "target/release"',
        )
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                mutated = self.text.replace(self.mod.PATH_MARKER, mutation, 1)
                with self.assertRaisesRegex(ValueError, "PATH"):
                    self.mod.validate(mutated)

    def test_rejects_readding_build_directories_to_path(self):
        anchor = '          $dlls | ForEach-Object { Write-Host "FFmpeg DLL: $($_.Name)" }\n'
        self.assertIn(anchor, self.text)
        mutated = self.text.replace(
            anchor, anchor + '          "$binDir" >> $env:GITHUB_PATH\n', 1)
        with self.assertRaisesRegex(ValueError, "PATH"):
            self.mod.validate(mutated)

    def test_rejects_unbound_stage(self):
        marker = '          "IRIS_VALIDATION_STAGE=$stage" >> $env:GITHUB_ENV\n'
        self.assertIn(marker, self.text)
        mutated = self.text.replace(marker, "", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)
        moved = self.text.replace(
            '          $stage = Join-Path $env:RUNNER_TEMP "iris-runtime-stage"\n',
            '          $stage = Join-Path $env:GITHUB_WORKSPACE "target/release"\n', 1)
        with self.assertRaises(ValueError):
            self.mod.validate(moved)

    def test_rejects_missing_smoke_exit_check(self):
        marker = '          if ($exitCode -ne 0) { throw "installed preview smoke failed with exit code $exitCode" }\n'
        self.assertIn(marker, self.text)
        mutated = self.text.replace(marker, "", 1)
        with self.assertRaises(ValueError):
            self.mod.validate(mutated)

    def test_rejects_smoke_outside_stage(self):
        for old, new in (
            ('$smokeExe = Join-Path $env:IRIS_VALIDATION_STAGE "preview_corpus_smoke.exe"',
             '$smokeExe = Join-Path $env:GITHUB_WORKSPACE "target/release/examples/preview_corpus_smoke.exe"'),
            ("& $smokeExe $corpus",
             "cargo run --release -p iris-thumb-preview --example preview_corpus_smoke"),
        ):
            with self.subTest(old=old):
                mutated = self.text.replace(old, new, 1)
                with self.assertRaises(ValueError):
                    self.mod.validate(mutated)

    def test_rejects_unattested_or_debug_stage(self):
        for old, new in (
            ("cargo build --release -p iris-ui-shell --locked",
             "cargo build -p iris-ui-shell --locked"),
            ('Copy-Item -LiteralPath $pdfium -Destination (Join-Path $stage "pdfium.dll") -Force',
             'Copy-Item -LiteralPath (Join-Path $env:RUNNER_TEMP "pdfium-extract/pdfium.dll") '
             '-Destination (Join-Path $stage "pdfium.dll") -Force'),
            ('Copy-Item -LiteralPath $rustcExe -Destination (Join-Path $stage "rustc.exe") -Force',
             'Write-Host "no compiler staged"'),
        ):
            with self.subTest(old=old):
                mutated = self.text.replace(old, new, 1)
                with self.assertRaises(ValueError):
                    self.mod.validate(mutated)

    def test_requires_bounded_evidence_in_manifest(self):
        for marker in self.mod.EVIDENCE_MARKERS:
            with self.subTest(marker=marker):
                self.assertIn(marker, self.text)
                mutated = self.text.replace(marker, "<removed>", 1)
                with self.assertRaises(ValueError):
                    self.mod.validate(mutated)


class WiringContracts(unittest.TestCase):
    def test_contract_wired_into_ci_and_workflow(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        wf = (ROOT / ".github/workflows/windows-ffmpeg-validation.yml").read_text()
        self.assertIn("python3 scripts/test-windows-ffmpeg-validation-workflow.py", ci)
        self.assertIn("python3 scripts/test-windows-ffmpeg-validation-workflow.py", wf)

    def test_workflow_registered_for_manual_dispatch(self):
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("uses: ./.github/workflows/windows-ffmpeg-validation.yml", ci)


if __name__ == "__main__":
    unittest.main(verbosity=2)
