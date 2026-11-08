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

    def test_liblzma_pin_and_signature_identity(self):
        self.assertEqual(m.LZMA['version'], '5.8.4')
        self.assertEqual(m.LZMA['url'], 'https://github.com/tukaani-project/xz/releases/download/v5.8.4/xz-5.8.4.tar.xz')
        self.assertEqual(m.LZMA['sha256'], '4ce24038fd4221e0d13bc1a2de7a4db56e90b92b3bf75321f6c14be73f65de4b')
        self.assertEqual(m.LZMA['signature_url'], m.LZMA['url'] + '.sig')
        self.assertEqual(m.LZMA['key_url'], 'https://github.com/Larhzu.gpg')
        self.assertEqual(m.LZMA['fingerprint'], '3690C240CE51B4670D30AD1C38EE757D69184620')
        self.assertEqual(m.LZMA['license'], dict(scope='liblzma', spdx='0BSD', metadata_file='COPYING',
                         file='COPYING.0BSD', sha256='0b01625d853911cd0e2e088dcfb743261034a091bb379246cb25a14cc4c74bf1'))
        primary = m.LZMA['fingerprint']
        valid = f'[GNUPG:] VALIDSIG {"A" * 40} 2026-01-01 1 0 4 0 1 10 00 {primary}\n'
        self.assertEqual(m.lzma_signature(valid)['primary_fingerprint'], primary)
        for bad in ('', valid + valid, valid.replace(primary, 'B' * 40),
                    valid + '[GNUPG:] BADSIG bad\n', valid + '[GNUPG:] EXPKEYSIG expired\n',
                    valid + '[GNUPG:] REVKEYSIG revoked\n'):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                m.lzma_signature(bad)

    def test_liblzma_build_contract_and_fail_closed_inputs(self):
        # Synthetic bytes test orchestration only; no upstream signature/native claims.
        for failure in (None, 'archive', 'key', 'signature', 'license', 'metadata', 'spdx', 'dylib'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as tmp:
                work = Path(tmp).resolve()
                docs = work / 'docs'
                docs.mkdir()
                archive = docs / 'xz-source.tar.xz'
                license = docs / 'xz-COPYING.0BSD'
                archive.write_text('synthetic archive')
                license.write_text('synthetic license')
                pin = dict(m.LZMA, sha256=m.digest(archive), license=dict(m.LZMA['license'], sha256=m.digest(license)))
                primary = pin['fingerprint']
                calls = []

                def run(*args, **kwargs):
                    args = tuple(str(a) for a in args)
                    calls.append(args)
                    if args[0] == 'curl' and failure == 'archive':
                        archive.write_text('corrupt')
                    if '--list-keys' in args:
                        return f"{'sub' if failure == 'key' else 'pub'}:-:255:22:x::::::\nfpr:::::::::{primary}:"
                    if '--verify' in args:
                        return '' if failure == 'signature' else f'[GNUPG:] VALIDSIG {primary} 2026-01-01 1 0 4 0 1 10 00 {primary}'
                    if args[0] == 'tar':
                        source = work / f"xz-{pin['version']}"
                        source.mkdir()
                        (source / 'COPYING').write_text('unknown' if failure == 'metadata' else
                                                       'liblzma is under the BSD Zero Clause License (0BSD).')
                        (source / 'COPYING.0BSD').write_text('corrupt' if failure == 'license' else 'synthetic license')
                    if args[:2] == ('cmake', '--install'):
                        prefix = work / 'liblzma'
                        (prefix / 'lib').mkdir(parents=True)
                        (prefix / 'include').mkdir()
                        (prefix / 'lib/liblzma.a').write_text('static')
                        (prefix / 'include/lzma.h').write_text('header')
                        if failure == 'dylib':
                            (prefix / 'lib/liblzma.dylib').touch()
                    return 'cmake version test'

                if failure == 'spdx':
                    pin['license']['spdx'] = 'unknown'
                with patch.object(m, 'LZMA', pin), patch.object(m, 'run', side_effect=run):
                    if failure:
                        with self.assertRaises(ValueError):
                            m.build_lzma(work, work / 'sdk', {'CC': 'clang'}, docs)
                        if failure != 'dylib':
                            self.assertFalse(any(c[0] == 'cmake' for c in calls))
                        if failure in ('archive', 'key', 'signature'):
                            self.assertFalse(any(c[0] == 'tar' for c in calls))
                        continue
                    prefix = m.build_lzma(work, work / 'sdk', {'CC': 'clang'}, docs)
                configure = next(c for c in calls if c[:2] == ('cmake', '-S'))
                for flag in ('-DBUILD_SHARED_LIBS=OFF', '-DCMAKE_POSITION_INDEPENDENT_CODE=ON',
                             '-DBUILD_TESTING=OFF', '-DXZ_NLS=OFF', '-DXZ_DOC=OFF',
                             '-DXZ_TOOL_XZ=OFF', '-DXZ_TOOL_XZDEC=OFF', '-DXZ_TOOL_LZMADEC=OFF',
                             '-DXZ_TOOL_LZMAINFO=OFF', '-DXZ_TOOL_SCRIPTS=OFF', '-DCMAKE_INSTALL_LIBDIR=lib'):
                    self.assertIn(flag, configure)
                self.assertIn(f'-DCMAKE_INSTALL_PREFIX={prefix}', configure)
                install = next(c for c in calls if c[:2] == ('cmake', '--install'))
                self.assertEqual(install[-2:], ('--component', 'liblzma_Development'))
                evidence = json.loads((docs / 'liblzma-build.json').read_text())
                self.assertTrue(evidence['signature']['verified'])
                self.assertEqual(evidence['static_archive_sha256'], m.digest(prefix / 'lib/liblzma.a'))
                self.assertEqual(evidence['cmake'], 'cmake version test')

    def test_candidate_rejects_ambient_liblzma_and_dynamic_edge(self):
        with patch.dict(m.os.environ, dict(CPATH='/opt/homebrew/include', LIBRARY_PATH='/opt/homebrew/lib',
                        CFLAGS='ambient', LDFLAGS='ambient', PKG_CONFIG_PATH='ambient',
                        PKG_CONFIG_LIBDIR='ambient', PKG_CONFIG_SYSROOT_DIR='ambient',
                        CMAKE_PREFIX_PATH='ambient', CPPFLAGS='ambient')):
            env = m.candidate_env(Path('/sdk'), '/clang')
        self.assertEqual(env['CFLAGS'], '-isysroot /sdk')
        self.assertEqual(env['LDFLAGS'], '-isysroot /sdk')
        for key in ('CPATH', 'LIBRARY_PATH', 'CMAKE_PREFIX_PATH', 'PKG_CONFIG_SYSROOT_DIR', 'CPPFLAGS'):
            self.assertNotIn(key, env)
        self.assertEqual(env['PKG_CONFIG_LIBDIR'], '')
        self.assertEqual(env['PKG_CONFIG_PATH'], '')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            exe = root / 'Iris'
            exe.touch()
            with patch.object(m, 'macho', return_value=(['/usr/lib/liblzma.5.dylib'], [], None)):
                with self.assertRaisesRegex(ValueError, 'statically linked'):
                    m.closure(exe, [], root / 'sdk')

    def test_ffmpeg_build_uses_only_pinned_liblzma_prefix(self):
        class Configured(Exception):
            pass
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp).resolve()
            lzma, sdk = work / 'liblzma', work / 'sdk'
            seen = []

            def run(*args, **kwargs):
                seen.append(args)
                if args[:2] == ('xcrun', '-f'):
                    return '/clang'
                if args[:2] == ('pkg-config', '--modversion'):
                    return m.LZMA['version']
                if args[:2] == ('pkg-config', '--variable=libdir'):
                    return str(lzma / 'lib')
                if '--list-keys' in args:
                    return f"fpr:::::::::{m.POLICY['fingerprint']}:"
                if '--verify' in args:
                    fp = m.POLICY['fingerprint']
                    return f'[GNUPG:] VALIDSIG {fp} 2026-01-01 1 0 4 0 1 10 00 {fp}'
                if args[0] == './configure':
                    self.assertEqual(list(args[3:]), m.POLICY['configure_flags'])
                    env = kwargs['env']
                    self.assertEqual(m.shlex.split(env['CFLAGS']), ['-isysroot', str(sdk), '-I' + str(lzma / 'include')])
                    self.assertEqual(m.shlex.split(env['LDFLAGS']), ['-isysroot', str(sdk), '-L' + str(lzma / 'lib'), '-liconv'])
                    for key in ('PKG_CONFIG_PATH', 'PKG_CONFIG_LIBDIR'):
                        self.assertEqual(env[key], str(lzma / 'lib/pkgconfig'))
                    raise Configured()
                return ''

            with patch.object(m, 'build_lzma', return_value=lzma), patch.object(m, 'digest', return_value=m.POLICY['sha256']), \
                    patch.object(m, 'run', side_effect=run), self.assertRaises(Configured):
                m.build(work / 'candidate', sdk)
            probe = next(c for c in seen if c[0] == '/clang')
            self.assertIn(lzma / 'lib/liblzma.a', probe)
            self.assertNotIn('-llzma', probe)

    def test_candidate_failure_cleans_all_scratch_and_writes_bounded_json(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            app = root / 'dist/macos/Iris.app'
            output = root / 'audit/candidate.json'

            def failed_build(prefix, sdk):
                prefix.mkdir()
                (prefix / 'source.tar.xz').write_text('temporary')
                (prefix.parent / 'cargo').mkdir()
                app.mkdir(parents=True)
                raise ValueError('synthetic prerequisite failure')

            with patch.object(m, 'ROOT', root), patch.object(m.sys, 'platform', 'darwin'), \
                    patch.object(m, 'run', return_value=str(root / 'sdk')), \
                    patch.object(m, 'build', side_effect=failed_build), \
                    patch.dict(m.REDACTIONS), self.assertRaisesRegex(ValueError, 'synthetic'):
                m.validate(output)
            self.assertFalse(app.parent.exists())
            self.assertFalse((root / 'target/macos-runtime-validation').exists())
            self.assertEqual(json.loads(output.read_text())['status'], 'failed')
            self.assertLessEqual(output.stat().st_size, 266240)

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

    def test_system_prefers_exact_sdk_stub_over_sanitizer_alias(self):
        with tempfile.TemporaryDirectory() as tmp:
            sdk = Path(tmp)
            libdir = sdk / 'usr/lib'
            libdir.mkdir(parents=True)
            canonical = libdir / 'libSystem.B.tbd'
            sanitizer = libdir / 'libSystem.B_asan.tbd'
            install = 'install-name: /usr/lib/libSystem.B.dylib\n'
            sanitizer.write_text(install)
            canonical.write_text(install)

            record = m.system(Path('/usr/lib/libSystem.B.dylib'), sdk)

            self.assertEqual(record['sdk_stub'], 'usr/lib/libSystem.B.tbd')
            self.assertEqual(record['sdk_stub_sha256'], m.digest(canonical))

            canonical.unlink()
            with self.assertRaisesRegex(ValueError, 'no matching SDK stub'):
                m.system(Path('/usr/lib/libSystem.B.dylib'), sdk)

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
            with self.assertRaisesRegex(ValueError, 'ambiguous') as caught:
                m.system(Path(install), sdk)
            self.assertIn(install, str(caught.exception))
            self.assertIn('CoreFoundation.tbd', str(caught.exception))
            self.assertIn('Versions/A/CoreFoundation.tbd', str(caught.exception))
            self.assertNotIn(str(sdk), str(caught.exception))
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
        self.assertIn('brew install ffmpeg pkg-config gnupg nasm cmake', workflow)
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
        self.assertLess(source.index('common.signature(status)'), source.index("run('tar'", source.index('def build(')))
        self.assertIn("CARGO_TARGET_DIR=str(target)", source)
        self.assertIn("PKG_CONFIG_LIBDIR=str(prefix / 'lib/pkgconfig')", source)
        self.assertIn("FFMPEG_DIR=str(prefix)", source)
        self.assertIn('shutil.rmtree(app.parent', source)
        self.assertIn('not the current release runtime', source)
        self.assertIn('not legal approval', source)


if __name__ == '__main__':
    unittest.main()
