#!/usr/bin/env python3
"""Deterministic Darwin contracts; Linux mocks are not native runtime evidence."""
import importlib.util
import json
from pathlib import Path
import tempfile
import sys
sys.dont_write_bytecode = True
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('macos_ffmpeg', ROOT / 'scripts/macos-ffmpeg.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class CandidateContracts(unittest.TestCase):
    def test_shared_pin_and_signature_refusal(self):
        self.assertEqual(m.POLICY, json.loads((ROOT / 'packaging/linux/ffmpeg.json').read_text()))
        self.assertEqual(m.MACOS_POLICY['platform'], 'macOS')
        self.assertEqual(m.MACOS_POLICY['source']['sha256'], m.POLICY['sha256'])
        self.assertNotIn('system_packages', m.MACOS_POLICY['source'])
        self.assertNotIn('system_sonames', m.MACOS_POLICY['source'])
        fingerprint = m.POLICY['fingerprint']
        valid = f'[GNUPG:] VALIDSIG {fingerprint} 2026-01-01 1 0 4 0 1 10 00 {fingerprint}\n'
        m.common.signature(valid)
        m.common.signature(valid.replace(fingerprint, 'A' * 40, 1))
        for bad in ('', valid + valid, valid.replace(fingerprint, 'A' * 40),
                    valid + '[GNUPG:] BADSIG bad\n', valid + '[GNUPG:] EXPKEYSIG expired\n'):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                m.common.signature(bad)

    def test_generated_and_native_configuration_refusal(self):
        config = '\n'.join(f'#define CONFIG_{k} {v}' for k, v in
                           dict(GPL=0, VERSION3=0, NONFREE=0, SHARED=1, STATIC=0,
                                PIC=1, ZLIB=1, BZLIB=1, LZMA=1, ICONV=1).items())
        codecs = '\n'.join(f'#define CONFIG_{k} 1' for k in m.POLICY['required_decoders'])
        m.generated_config(config, codecs)
        for line in config.splitlines():
            with self.subTest(line=line), self.assertRaises(ValueError):
                m.generated_config(config.replace(line, ''), codecs)
        with self.assertRaises(ValueError):
            m.generated_config(config.replace('CONFIG_GPL 0', 'CONFIG_GPL 1'), codecs)
        with self.assertRaises(ValueError):
            m.generated_config(config, codecs.replace('CONFIG_PNG_DECODER 1', 'CONFIG_PNG_DECODER 0'))
        flags = ' '.join(m.POLICY['configure_flags'])
        self.assertEqual(m.selected_flags('--prefix=/private --libdir=/private/lib ' + flags),
                         m.POLICY['configure_flags'])
        for bad in (flags + ' --enable-gpl', flags.replace('--enable-lzma', '--disable-lzma')):
            with self.assertRaises(ValueError):
                m.selected_flags(bad)

    def test_load_commands_exclude_id_include_weak_and_rpaths(self):
        text = '''example:
Load command 0
          cmd LC_ID_DYLIB
         name @rpath/liba.1.dylib (offset 24)
Load command 1
          cmd LC_LOAD_WEAK_DYLIB
         name @rpath/libb.1.dylib (offset 24)
Load command 2
          cmd LC_RPATH
         path @loader_path/../Frameworks (offset 12)
'''
        with patch.object(m, 'run', return_value=text):
            self.assertEqual(m.macho(Path('liba')), (['@rpath/libb.1.dylib'],
                             ['@loader_path/../Frameworks'], '@rpath/liba.1.dylib'))
        with patch.object(m, 'run', return_value=text.replace('LC_LOAD_WEAK_DYLIB', 'LC_UNKNOWN_DYLIB')):
            with self.assertRaises(ValueError):
                m.macho(Path('liba'))

    def test_system_matches_exact_install_name_not_filename(self):
        with tempfile.TemporaryDirectory() as tmp:
            sdk = Path(tmp)
            # Stub filename differs from the versioned install name it provides.
            stub = sdk / 'usr/lib/libz.tbd'
            stub.parent.mkdir(parents=True)
            stub.write_text("install-name: /usr/lib/libz.1.dylib\n")
            record = m.system(Path('/usr/lib/libz.1.dylib'), sdk)
            self.assertEqual(record['sdk_stub'], 'usr/lib/libz.tbd')
            self.assertEqual(record['sdk_stub_sha256'], m.digest(stub))
            self.assertIsNone(m.system(Path('/opt/homebrew/lib/libz.1.dylib'), sdk))
            with self.assertRaises(ValueError):
                m.system(Path('/usr/lib/libunknown.dylib'), sdk)
            stub.write_text('# install-name: /usr/lib/libz.1.dylib\n')
            with self.assertRaises(ValueError):
                m.system(Path('/usr/lib/libz.1.dylib'), sdk)

    def test_system_framework_stub_and_ambiguous_or_missing_fail_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            sdk = Path(tmp)
            framework = sdk / 'System/Library/Frameworks/CoreFoundation.framework'
            versioned = framework / 'Versions/A'
            versioned.mkdir(parents=True)
            install = ('/System/Library/Frameworks/CoreFoundation.framework/'
                       'Versions/A/CoreFoundation')
            stub = framework / 'CoreFoundation.tbd'
            stub.write_text(f"install-name: {install}\n")
            record = m.system(Path(install), sdk)
            self.assertEqual(record['sdk_stub'],
                             'System/Library/Frameworks/CoreFoundation.framework/CoreFoundation.tbd')
            self.assertEqual(record['sdk_stub_sha256'], m.digest(stub))
            # A second stub claiming the same install name is ambiguous.
            duplicate = versioned / 'CoreFoundation.tbd'
            duplicate.write_text(f"install-name: {install}\n")
            with self.assertRaisesRegex(ValueError, 'ambiguous'):
                m.system(Path(install), sdk)
            duplicate.unlink()
            with self.assertRaises(ValueError):
                m.system(Path('/System/Library/Frameworks/Missing.framework/Versions/A/Missing'), sdk)
            self.assertIsNone(m.system(Path('/opt/homebrew/lib/libz.1.dylib'), sdk))

    def test_loader_executable_and_inherited_runpaths_with_cycle(self):
        with tempfile.TemporaryDirectory() as tmp:
            # closure resolves its loader paths; mock keys must match on macOS,
            # where TemporaryDirectory may live behind /var -> /private/var.
            root = Path(tmp).resolve()
            exe, a, b = root / 'MacOS/Iris', root / 'Frameworks/liba.dylib', root / 'Frameworks/libb.dylib'
            for path in (exe, a, b):
                path.parent.mkdir(exist_ok=True)
                path.touch()
            graph = {exe: (['@rpath/liba.dylib'], ['@executable_path/../Frameworks'], None),
                     a: (['@rpath/libb.dylib'], [], None),
                     b: (['@loader_path/liba.dylib'], [], None)}
            with patch.object(m, 'macho', side_effect=graph.__getitem__):
                edges = m.closure(exe, [a, b], root / 'sdk')
                self.assertEqual([e[2] for e in edges], [a, b, a])
                b.unlink()
                with self.assertRaisesRegex(ValueError, 'unresolved'):
                    m.closure(exe, [a, b], root / 'sdk')

    def test_missing_tokens_external_first_match_and_basename_collision_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            exe, a, other = root / 'Iris', root / 'one/liba.dylib', root / 'two/liba.dylib'
            for path in (exe, a, other):
                path.parent.mkdir(exist_ok=True)
                path.touch()
            with patch.object(m, 'macho', return_value=(['@rpath/missing.dylib'], [], None)):
                with self.assertRaisesRegex(ValueError, 'unresolved'):
                    m.closure(exe, [a], root / 'sdk')
            graph = {exe: (['@rpath/liba.dylib'], [str(other.parent), str(a.parent)], None)}
            with patch.object(m, 'macho', side_effect=graph.__getitem__):
                with self.assertRaisesRegex(ValueError, 'outside exact'):
                    m.closure(exe, [a], root / 'sdk')
            graph = {exe: ([str(a), str(other)], [], None), a: ([str(a)], [], None), other: ([str(other)], [], None)}
            with patch.object(m, 'macho', side_effect=graph.__getitem__):
                with self.assertRaisesRegex(ValueError, 'basename collision'):
                    m.closure(exe, [a, other], root / 'sdk')

    def test_library_inventory_rejects_extra_dylibs_and_static_archives(self):
        with tempfile.TemporaryDirectory() as tmp:
            prefix = Path(tmp)
            libdir = prefix / 'lib'
            libdir.mkdir()
            for name in m.POLICY['libraries']:
                real = libdir / f'lib{name}.1.2.3.dylib'
                real.touch()
                (libdir / f'lib{name}.dylib').symlink_to(real.name)
            self.assertEqual(set(m.libraries(prefix)), set(m.POLICY['libraries']))
            extra = libdir / 'libunexpected.1.dylib'
            extra.touch()
            with self.assertRaisesRegex(ValueError, 'unknown candidate dylib'):
                m.libraries(prefix)
            extra.unlink()
            (libdir / 'libavcodec.a').touch()
            with self.assertRaisesRegex(ValueError, 'unexpected static candidate'):
                m.libraries(prefix)

    def test_runtime_uses_staged_functions_and_refuses_license_abi_flags(self):
        class Function:
            def __init__(self, value):
                self.value = value
            def __call__(self):
                return self.value
        class Library:
            def __getattr__(self, name):
                return Function(values[name.rsplit('_', 1)[1]])
        values = dict(configuration=' '.join(m.POLICY['configure_flags']).encode(),
                      license=b'LGPL version 2.1 or later', version=(62 << 16) | (28 << 8) | 103)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'libavcodec.62.dylib'
            path.touch()
            with patch.object(m.ctypes, 'CDLL', return_value=Library()) as cdll:
                self.assertEqual(m.runtime({'avcodec': path})[0]['version'], [62, 28, 103])
                cdll.assert_called_once_with(str(path))
                for key, bad in (('license', b'GPL'), ('version', 0), ('configuration', b'--enable-gpl')):
                    good = values[key]
                    values[key] = bad
                    with self.subTest(key=key), self.assertRaises(ValueError):
                        m.runtime({'avcodec': path})
                    values[key] = good

    def test_report_only_workflow_keeps_current_and_candidate_json_only(self):
        workflow = (ROOT / '.github/workflows/macos-runtime-validation.yml').read_text()
        # The current release path still uses Homebrew FFmpeg only to document
        # the unchanged release runtime; the candidate never uses Homebrew.
        self.assertIn('brew install ffmpeg', workflow)
        self.assertIn('./scripts/package-macos-app.sh', workflow)
        self.assertIn('python3 scripts/release-audit.py macos --output audit/macos.json', workflow)
        self.assertIn('macos-ffmpeg.py validate audit/macos-candidate.json', workflow)
        self.assertNotIn('actions/cache', workflow)
        self.assertNotIn('GH_TOKEN', workflow)
        self.assertEqual(workflow.count('actions/upload-artifact@'), 1)
        self.assertIn('audit/macos.json', workflow)
        self.assertIn('audit/macos-candidate.json', workflow)
        self.assertIn("trap 'rm -rf dist/macos target/package-macos' EXIT", workflow)
        self.assertIn("trap 'rm -rf dist/macos target/macos-runtime-validation' EXIT", workflow)
        self.assertIn('rm -rf dist/macos target/package-macos target/macos-runtime-validation', workflow)
        self.assertNotIn('dist/macos/Iris', workflow.split('upload-artifact')[-1])
        source = (ROOT / 'scripts/macos-ffmpeg.py').read_text()
        self.assertLess(source.index('common.signature(status)'), source.index("run('tar'"))
        self.assertIn("CARGO_TARGET_DIR=str(target)", source)
        self.assertIn("PKG_CONFIG_LIBDIR=str(prefix / 'lib/pkgconfig')", source)
        self.assertIn("FFMPEG_DIR=str(prefix)", source)
        self.assertIn('shutil.rmtree(app.parent', source)
        self.assertIn('not the current release runtime', source)
        self.assertIn('not legal approval', source)


if __name__ == '__main__':
    unittest.main()
