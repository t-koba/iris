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
    def test_pinned_download_retries_are_configurable_and_bounded(self):
        with patch.dict(m.os.environ, {
            'IRIS_FFMPEG_FETCH_RETRIES': '1',
            'IRIS_FFMPEG_FETCH_RETRY_DELAY_SECONDS': '3',
        }), patch.object(m, 'run') as run:
            m.download_pinned('https://example.invalid/pinned.tar.xz', Path('/tmp/pinned.tar.xz'))
        args = run.call_args.args
        self.assertIn('--remove-on-error', args)
        self.assertEqual(args[args.index('--retry') + 1], '1')
        self.assertEqual(args[args.index('--retry-delay') + 1], '3')
        self.assertIn('https://example.invalid/pinned.tar.xz', args)
        self.assertEqual(run.call_args.kwargs['seconds'], m.setting('IRIS_FFMPEG_FETCH_SECONDS', 300))
        with patch.dict(m.os.environ, {'IRIS_FFMPEG_FETCH_RETRIES': '0'}), patch.object(m, 'run') as run:
            m.download_pinned('https://example.invalid/pinned.tar.xz', Path('/tmp/pinned.tar.xz'))
        self.assertNotIn('--retry', run.call_args.args)
        for name, value in (('IRIS_FFMPEG_FETCH_RETRIES', '-1'),
                            ('IRIS_FFMPEG_FETCH_RETRY_DELAY_SECONDS', '-1')):
            with patch.dict(m.os.environ, {name: value}), self.assertRaises(ValueError):
                m.download_pinned('https://example.invalid/pinned.tar.xz', Path('/tmp/pinned.tar.xz'))

    def test_command_cancellation_is_not_an_optional_comparison_failure(self):
        for error in (InterruptedError('TERM'), KeyboardInterrupt(), m.subprocess.TimeoutExpired('synthetic', 1)):
            with self.subTest(error=type(error).__name__), patch.object(m.subprocess, 'Popen') as popen, \
                    patch.object(m.os, 'killpg') as kill:
                process = popen.return_value
                process.pid = 123
                process.wait.side_effect = [error, 0]
                with self.assertRaises(ValueError if isinstance(error, m.subprocess.TimeoutExpired) else type(error)):
                    m.run('synthetic')
                kill.assert_called_once_with(123, m.signal.SIGKILL)
                self.assertEqual(process.wait.call_count, 2)

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
                    patch.object(m, 'parse_preview', return_value={}), \
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
        # Historical Homebrew comparison remains explicitly report-only.
        self.assertIn('IRIS_MACOS_REPORT_ONLY=1 IRIS_MACOS_FFMPEG_BASELINE=1', workflow)
        self.assertIn('brew install ffmpeg pkg-config gnupg nasm cmake', workflow)
        self.assertIn('./scripts/package-macos-app.sh', workflow)
        self.assertIn('Audit pinned production package', workflow)
        production_step = workflow.split('      - name: Audit pinned production package', 1)[1].split(
            '      - name: Validate pinned candidate', 1)[0]
        self.assertIn("if: ${{ !cancelled() && steps.candidate_contracts.outcome == 'success' }}", production_step)
        self.assertNotIn('success()', production_step)
        self.assertIn('python3 scripts/macos-ffmpeg.py check dist/macos/Iris.app', workflow)
        self.assertIn('python3 scripts/release-audit.py macos --output audit/macos-product.json', workflow)
        self.assertIn('python3 scripts/release-audit.py macos --output audit/macos.json', workflow)
        self.assertIn('macos-ffmpeg.py validate audit/macos-candidate.json', workflow)
        self.assertNotIn('actions/cache', workflow)
        self.assertNotIn('GH_TOKEN', workflow)
        self.assertEqual(workflow.count('actions/upload-artifact@'), 1)
        self.assertIn('audit/macos.json', workflow)
        self.assertIn('audit/macos-product.json', workflow)
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


class ProductRuntime(unittest.TestCase):
    def test_release_selector_and_archive_signing_order(self):
        release = (ROOT / '.github/workflows/release.yml').read_text().split('  package-linux:')[0]
        self.assertNotIn('brew install ffmpeg', release)
        self.assertIn('brew install pkg-config gnupg nasm cmake', release)
        self.assertIn('cargo fetch --locked', release)
        self.assertIn('cache-targets: false', release)
        for name in ('IRIS_PREVIEW_CORPUS_MAX_BYTES', 'IRIS_PREVIEW_CORPUS_TIMEOUT_SECONDS',
                     'IRIS_PREVIEW_LATENCY_WARMUPS', 'IRIS_PREVIEW_LATENCY_SAMPLES'):
            self.assertIn(name, release)
        self.assertIn('macos-ffmpeg.py check dist/macos/Iris.app', release)
        shell = (ROOT / 'scripts/package-macos-app.sh').read_text()
        self.assertIn('"$baseline" == 1 && "${IRIS_MACOS_REPORT_ONLY:-}" == 1', shell)
        self.assertIn('"$sign_identity" == - && -z "$notary_profile"', shell)
        steps = ['macos-ffmpeg.py" product', 'codesign "${codesign_args[@]}"',
                 'macos-ffmpeg.py" check', 'ditto -c', 'notarytool submit', 'stapler staple', 'pkgbuild --component']
        self.assertEqual(sorted(shell.index(step) for step in steps), [shell.index(step) for step in steps])
        self.assertNotIn('--deep', shell.split('codesign_args=(')[1].split(')')[0])

    def test_signing_rebinds_only_verified_bytes_and_fails_closed(self):
        for identity, failure in (('-', None), ('Developer ID synthetic', None),
                                  ('-', 'tamper'), ('-', 'sign'), ('-', 'verify')):
            with self.subTest(identity=identity, failure=failure), tempfile.TemporaryDirectory() as tmp:
                app = Path(tmp)
                docs, frameworks = app / 'Contents/Resources/ffmpeg', app / 'Contents/Frameworks'
                docs.mkdir(parents=True)
                frameworks.mkdir()
                lib = frameworks / 'libavcodec.1.dylib'
                lib.write_text('verified bytes')
                manifest = docs / 'manifest.json'
                original = json.dumps(dict(staged_libraries={lib.name: m.digest(lib)}))
                manifest.write_text(original)
                if failure == 'tamper':
                    lib.write_text('unverified replacement')
                def run(*args):
                    if (failure == 'sign' and '--sign' in args) or (failure == 'verify' and '--verify' in args):
                        raise ValueError('synthetic signing failure')
                    if '--sign' in args:
                        lib.write_text('signed bytes')
                with patch.object(m, 'run', side_effect=run) as command:
                    if failure:
                        with self.assertRaises(ValueError):
                            m.sign_runtime(app, identity)
                        self.assertEqual(manifest.read_text(), original)
                        if failure == 'tamper':
                            command.assert_not_called()
                    else:
                        m.sign_runtime(app, identity)
                        self.assertEqual(json.loads(manifest.read_text())['staged_libraries'], {lib.name: m.digest(lib)})
                        self.assertEqual('--options' in command.call_args_list[0].args, identity != '-')

    def test_product_requires_corpus_staged_closure_and_audit_and_cleans_scratch(self):
        for failure in (None, 'build', 'closure', 'fetch', 'smoke', 'sign', 'audit', 'interrupt'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp).resolve()
                app = root / 'Iris.app'
                macos = app / 'Contents/MacOS'
                macos.mkdir(parents=True)
                (app / 'Contents/Frameworks').mkdir()
                (app / 'Contents/Resources').mkdir()
                envs, calls = [], []
                def build(prefix, sdk):
                    if failure == 'build':
                        raise ValueError('bad source/signature/config')
                def preview(env):
                    envs.append(env)
                    path = Path(env['CARGO_TARGET_DIR']) / 'release/examples/preview_corpus_smoke'
                    path.parent.mkdir(parents=True)
                    path.write_text('smoke')
                    return path
                def run(*args, **kwargs):
                    calls.append(tuple(str(a) for a in args))
                    if args[0] == 'cargo':
                        self.assertNotIn('--no-default-features', args)
                        self.assertIn('--offline', args)
                        (Path(kwargs['env']['CARGO_TARGET_DIR']) / 'release/Iris').write_text('product')
                    elif len(args) > 1 and str(args[1]).endswith('fetch-preview-corpus.py'):
                        if failure == 'fetch':
                            raise ValueError('corpus unavailable')
                    elif args[0] == macos / 'preview_corpus_smoke':
                        if failure == 'interrupt':
                            raise InterruptedError('cancelled')
                        if failure == 'smoke':
                            raise ValueError('incorrect preview')
                        return 'synthetic preview records'
                    return ''
                def edges(*args, **kwargs):
                    if failure == 'closure':
                        raise ValueError('external Homebrew edge')
                    return [(macos / 'Iris', '@loader_path/libavcodec.1.dylib',
                             app / 'Contents/Frameworks/libavcodec.1.dylib')]
                with patch.object(m, 'ROOT', root), patch.object(m.sys, 'platform', 'darwin'), \
                        patch.dict(m.os.environ, {'GITHUB_SHA': 'synthetic', 'FFMPEG_DIR': '/untrusted',
                                                 'IRIS_MACOS_SIGN_IDENTITY': '-'}, clear=True), \
                        patch.object(m, 'build', side_effect=build), patch.object(m, 'build_preview', side_effect=preview), \
                        patch.object(m, 'run', side_effect=run), patch.object(m, 'stage') as stage, \
                        patch.object(m, 'closure', side_effect=edges), \
                        patch.object(m, 'parse_preview', return_value={'correctness': True}), \
                        patch.object(m, 'sign_runtime', side_effect=ValueError('sign failed') if failure == 'sign' else None), \
                        patch.object(m, 'check', side_effect=ValueError('audit failed') if failure == 'audit' else None,
                                     return_value={'executables': {}, 'runtime': []}) as check:
                    if failure:
                        with self.assertRaises((ValueError, InterruptedError)):
                            m.product(app, root / 'sdk')
                        self.assertFalse((app / 'Contents/Resources/ffmpeg-runtime.json').exists())
                    else:
                        m.product(app, root / 'sdk')
                        report = json.loads((app / 'Contents/Resources/ffmpeg-runtime.json').read_text())
                        self.assertEqual(report['preview'], {'correctness': True})
                        self.assertNotIn('executables', report)
                        self.assertFalse((macos / 'preview_corpus_smoke').exists())
                        self.assertTrue((macos / 'Iris').exists())
                        stage.assert_called_once()
                        check.assert_called_once()
                        self.assertTrue(any('fetch-preview-corpus.py' in ' '.join(c) for c in calls))
                        self.assertNotEqual(envs[0]['FFMPEG_DIR'], '/untrusted')
                        self.assertEqual(envs[0]['PKG_CONFIG_LIBDIR'], envs[0]['FFMPEG_DIR'] + '/lib/pkgconfig')
                    self.assertEqual(list((root / 'target').iterdir()), [])

    def test_stage_refuses_changed_provenance_before_copying(self):
        for failure in (None, 'policy', 'source', 'notice', 'library', 'external'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as tmp:
                prefix, app = Path(tmp) / 'prefix', Path(tmp) / 'app'
                docs = prefix / 'share/iris-ffmpeg'
                docs.mkdir(parents=True)
                library = prefix / 'libavcodec.1.dylib'
                library.write_text('synthetic native library')
                (docs / 'source.tar.xz').write_text('synthetic pinned source')
                (docs / 'signature.status').write_text('synthetic signature fixture')
                (docs / 'config.h').write_text('synthetic generated configuration')
                (docs / 'config_components.h').write_text('synthetic generated codecs')
                (docs / 'COPYING.LGPLv2.1').write_text('synthetic notice')
                manifest = dict(policy=m.MACOS_POLICY, signature_verified=True, liblzma={}, enabled_codecs=[],
                                documents={p.name: m.digest(p) for p in docs.iterdir()},
                                libraries={library.name: m.digest(library)})
                pin = dict(m.POLICY, sha256=m.digest(docs / 'source.tar.xz'))
                if failure == 'policy':
                    manifest['policy'] = {}
                elif failure in ('source', 'notice'):
                    (docs / ('source.tar.xz' if failure == 'source' else 'COPYING.LGPLv2.1')).write_text('changed')
                elif failure == 'library':
                    library.write_text('unverified bytes')
                (docs / 'manifest.json').write_text(json.dumps(manifest))
                frameworks = app / 'Contents/Frameworks'
                frameworks.mkdir(parents=True)
                (app / 'Contents/MacOS').mkdir()
                (app / 'Contents/MacOS/Iris').touch()
                with patch.object(m, 'POLICY', pin), patch.object(m, 'libraries', return_value={'avcodec': library}), \
                        patch.object(m.common, 'signature'), patch.object(m, 'lzma_evidence', return_value={}), \
                        patch.object(m, 'generated_config', return_value=[]), patch.object(m, 'run'), \
                        patch.object(m, 'closure', return_value=[],
                                     side_effect=ValueError('external dependency') if failure == 'external' else None):
                    if failure:
                        with self.assertRaises(ValueError):
                            m.stage(prefix, app, Path(tmp) / 'sdk')
                        self.assertEqual(list(frameworks.iterdir()), [])
                    else:
                        m.stage(prefix, app, Path(tmp) / 'sdk')
                        self.assertEqual((frameworks / library.name).read_bytes(), library.read_bytes())
                        self.assertTrue((app / 'Contents/Resources/ffmpeg/source.tar.xz').is_file())

    def test_product_refuses_non_native_and_dyld_or_report_only_overrides(self):
        for platform, env in (('linux', {}), ('darwin', {'DYLD_LIBRARY_PATH': '/external'}),
                              ('darwin', {'IRIS_MACOS_REPORT_ONLY': '1'})):
            with patch.object(m.sys, 'platform', platform), patch.dict(m.os.environ, env, clear=True), \
                    patch.object(m, 'build') as build, self.assertRaises(ValueError):
                m.product(Path('/synthetic/app'), Path('/synthetic/sdk'))
            build.assert_not_called()


class PreviewComparison(unittest.TestCase):
    def setUp(self):
        self.env = patch.dict(m.os.environ, IRIS_PREVIEW_LATENCY_WARMUPS='2', IRIS_PREVIEW_LATENCY_SAMPLES='21')
        self.env.start()
        self.addCleanup(self.env.stop)
        self.identity = dict.fromkeys((
            'source_revision', 'workflow_run_id', 'cargo_lock_sha256', 'rustc', 'target_arch',
            'runner_name', 'runner_os', 'runner_arch', 'runner_image_os', 'runner_image_version',
            'cpu_model', 'available_parallelism', 'release_opt_level', 'crate', 'crate_version'), 'synthetic')
        self.identity.update(target_os='macos', build_profile='release',
                             features=dict(video=True, pdf=True, raw_full=True))
        pins = json.loads((ROOT / 'tests/preview-corpus/fixtures.json').read_text())['files']
        self.correct = dict(record='iris.preview_correctness.v1', passed=True, identity=self.identity,
                            fixtures=[dict(filename=p['filename'], sha256=p['sha256'], size_bytes=p['size'],
                                           output=dict(width=256, height=144, format='jpeg', source='synthetic')) for p in pins])
        fixtures = {f['filename']: f for f in self.correct['fixtures']}
        self.timings = [dict(record='iris.preview_latency.v1', operation=op, uses_ffmpeg=ffmpeg,
                            input={k: fixtures[name][k] for k in ('filename', 'sha256', 'size_bytes')},
                            output=dict(fixtures[name]['output'], bytes=123), identity=self.identity,
                            timing=dict(unit='nanoseconds', warmup_iterations=2, sample_count=21,
                                        min=1, median=2, p90=3, max=4, median_absolute_deviation=1))
                        for op, (name, ffmpeg) in m.PREVIEW_OPERATIONS.items()]

    def text(self):
        return ('discarded correctness diagnostics\nPREVIEW_CORRECTNESS ' + json.dumps(self.correct) + '\n' +
                '\n'.join('PREVIEW_LATENCY ' + json.dumps(t) for t in self.timings))

    def parsed(self):
        return m.parse_preview(self.text(), 'synthetic')

    def test_records_pair_by_operation_not_order_without_threshold(self):
        baseline = self.parsed()
        self.timings.reverse()
        for timing in self.timings:
            timing['timing'].update(median=200, p90=300, max=400)
        candidate = self.parsed()
        pairs = m.paired_preview(baseline, candidate)
        self.assertEqual(len(pairs), 3)
        self.assertEqual(sum(p['uses_ffmpeg'] for p in pairs), 1)
        self.assertTrue(all(p['candidate_minus_baseline_ns'] == 198 for p in pairs))
        self.assertNotIn('discarded', json.dumps(baseline))
        self.assertNotIn('threshold', json.dumps(pairs))
        # A representative retained comparison is well below the existing cap.
        self.assertLess(len(m.report_bytes(dict(baseline=baseline, candidate=candidate, pairs=pairs))), 266240)

    def test_parse_rejects_incomplete_duplicate_malformed_and_oversized_records(self):
        text = self.text()
        for bad in ('', text.rsplit('\n', 1)[0], text + '\n' + text,
                    text.replace('PREVIEW_LATENCY {', 'PREVIEW_LATENCY !{', 1)):
            with self.subTest(bad=bad[:40]), self.assertRaises(ValueError):
                m.parse_preview(bad, 'synthetic')
        with patch.dict(m.os.environ, IRIS_FFMPEG_COMMAND_BYTES=str(len(text.encode()) - 1)):
            with self.assertRaisesRegex(ValueError, 'budget'):
                self.parsed()

    def test_parse_rejects_unpinned_or_invalid_measurements(self):
        mutations = [
            (self.correct, 'passed', False),
            (self.correct['fixtures'][0], 'sha256', '0' * 64),
            (self.correct['fixtures'][0], 'size_bytes', 1),
            (self.correct['fixtures'][0]['output'], 'width', 257),
            (self.timings[0]['input'], 'sha256', '0' * 64),
            (self.timings[0], 'operation', 'unknown'),
            (self.timings[0], 'uses_ffmpeg', True),
            (self.timings[0]['output'], 'height', 1),
            (self.timings[0]['timing'], 'sample_count', 22),
            (self.timings[0]['timing'], 'warmup_iterations', 3),
            (self.timings[0]['timing'], 'median', -1),
            (self.timings[0]['timing'], 'max', float('nan')),
            (self.timings[0]['timing'], 'max', 1 << 64),
            (self.identity, 'source_revision', 'other-source'),
            (self.identity, 'cpu_model', None),
            (self.identity['features'], 'video', False),
        ]
        for obj, key, bad in mutations:
            good = obj[key]
            obj[key] = bad
            with self.subTest(key=key, bad=bad), self.assertRaises(ValueError):
                self.parsed()
            obj[key] = good

    def test_pair_rejects_each_experiment_identity_mismatch(self):
        baseline = self.parsed()
        # These records can each be valid independently, but not paired.
        for key in self.identity:
            if key in ('target_os', 'build_profile', 'features', 'source_revision'):
                continue  # Independently rejected above; source is also checked below.
            old = self.identity[key]
            self.identity[key] = 'different'
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, 'identity mismatch'):
                m.paired_preview(baseline, self.parsed())
            self.identity[key] = old
        import copy
        for section, key, bad in (('input', 'sha256', '0' * 64), ('input', 'size_bytes', 0),
                                  ('output', 'width', 128), ('output', 'height', 128),
                                  ('output', 'source', 'different'), ('identity', 'source_revision', 'other'),
                                  ('identity', 'features', {}), ('timing', 'sample_count', 1)):
            candidate = copy.deepcopy(baseline)
            candidate['timings'][0][section][key] = bad
            with self.subTest(section=section, key=key), self.assertRaises(ValueError):
                m.paired_preview(baseline, candidate)
        candidate = copy.deepcopy(baseline)
        candidate['timings'][0]['operation'] = 'other'
        with self.assertRaises(ValueError):
            m.paired_preview(baseline, candidate)

    def test_existing_report_cap_and_nonfinite_rejection(self):
        data = m.report_bytes({'synthetic': 'evidence'})
        with patch.dict(m.os.environ, IRIS_FFMPEG_MAX_REPORT_BYTES=str(len(data.encode()))):
            self.assertEqual(m.report_bytes({'synthetic': 'evidence'}), data)
        with patch.dict(m.os.environ, IRIS_FFMPEG_MAX_REPORT_BYTES=str(len(data.encode()) - 1)):
            with self.assertRaisesRegex(ValueError, 'budget'):
                m.report_bytes({'synthetic': 'evidence'})
        with self.assertRaises(ValueError):
            m.report_bytes({'bad': float('nan')})

    def test_fresh_default_feature_builds_and_pinned_discovery(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            overrides = dict.fromkeys(('CFLAGS', 'CXXFLAGS', 'CPPFLAGS', 'LDFLAGS', 'LIBS', 'OBJC_INCLUDE_PATH',
                                       'PKG_CONFIG_SYSROOT_DIR', 'PKG_CONFIG', 'CPATH', 'C_INCLUDE_PATH',
                                       'CPLUS_INCLUDE_PATH', 'LIBRARY_PATH', 'CMAKE_PREFIX_PATH',
                                       'CMAKE_TOOLCHAIN_FILE', 'FFMPEG_LIB_DIR', 'IRIS_MACOS_FFMPEG_CANDIDATE'),
                                      '/untrusted')
            with patch.dict(m.os.environ, overrides):
                baseline = m.preview_env(root / 'brew', root / 'cargo-baseline', 'synthetic')
                candidate = m.preview_env(root / 'candidate', root / 'cargo-candidate', 'synthetic')
            for env in (baseline, candidate):
                self.assertFalse(overrides.keys() & env.keys())
            self.assertNotEqual(baseline['CARGO_TARGET_DIR'], candidate['CARGO_TARGET_DIR'])
            for env in (baseline, candidate):
                with patch.object(m, 'run', side_effect=[env['FFMPEG_DIR'], '']) as run:
                    m.build_preview(env)
                    args = run.call_args[0]
                    self.assertIn('--locked', args)
                    self.assertIn('--offline', args)
                    self.assertNotIn('--no-default-features', args)
                    self.assertEqual(run.call_args.kwargs['env'], env)
                with patch.object(m, 'run', return_value=str(root / 'wrong')):
                    with self.assertRaisesRegex(ValueError, 'another runtime'):
                        m.build_preview(env)
            (root / 'cargo-baseline').mkdir()
            with self.assertRaisesRegex(ValueError, 'fresh'):
                m.preview_env(root / 'brew', root / 'cargo-baseline', 'synthetic')


    def test_single_shared_fetch_and_cleanup_on_success_failure_and_interruption(self):
        cases = [(None, failure) for failure in (None, ValueError, KeyboardInterrupt, InterruptedError)]
        cases += [(step, None) for step in ('fetch', 'baseline-discovery', 'baseline-build', 'baseline-smoke')]
        cases += [('baseline-build', ValueError), ('fetch', 'inventory'), ('fetch', 'generated')]
        for unavailable, failure in cases:
            with self.subTest(unavailable=unavailable, failure=failure), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp).resolve()
                pins = root / 'tests/preview-corpus/fixtures.json'
                pins.parent.mkdir(parents=True)
                pins.write_bytes((ROOT / 'tests/preview-corpus/fixtures.json').read_bytes())
                app = root / 'dist/macos/Iris.app'
                output = root / 'audit/candidate.json'
                fetches, measurements, builds, candidate_checks = [], [], [], []

                def optional_failure(step):
                    if unavailable == step:
                        raise ValueError(str(fetches[0].parent) + ' synthetic ' + step + ' failure ' + 'x' * 40000)

                def run(*args, **kwargs):
                    command = str(args[0])
                    env = kwargs.get('env', {})
                    if command == 'xcrun':
                        return str(root / 'sdk')
                    if command == 'brew':
                        optional_failure('baseline-discovery')
                        return str(root / 'brew')
                    if command == 'pkg-config':
                        return env['FFMPEG_DIR']
                    if command.endswith('fetch-preview-corpus.py') or (
                            len(args) > 1 and str(args[1]).endswith('fetch-preview-corpus.py')):
                        corpus = Path(args[-1])
                        corpus.mkdir()
                        (corpus / 'synthetic-fixture').write_text('must be removed')
                        fetches.append(corpus)
                        optional_failure('fetch')
                    if command == 'cargo' and 'preview_corpus_smoke' in args:
                        if env['FFMPEG_DIR'].endswith('/brew'):
                            optional_failure('baseline-build')
                        target = Path(env['CARGO_TARGET_DIR'])
                        self.assertFalse(target.exists())
                        (target / 'release/examples').mkdir(parents=True)
                        (target / 'release/examples/preview_corpus_smoke').touch()
                        builds.append(env)
                    if command.endswith('package-macos-app.sh'):
                        for directory in ('MacOS', 'Frameworks'):
                            (app / 'Contents' / directory).mkdir(parents=True)
                        (app / 'Contents/MacOS/Iris').touch()
                        (app / 'Contents/MacOS/video_thumbnail_smoke').touch()
                    if command.endswith('preview_corpus_smoke'):
                        self.assertTrue((Path(args[1]) / 'synthetic-fixture').is_file())
                        measurements.append(Path(args[1]))
                        if env['FFMPEG_DIR'].endswith('/brew'):
                            optional_failure('baseline-smoke')
                        elif failure and not isinstance(failure, str):
                            raise failure('synthetic smoke interruption/failure')
                        return self.text()
                    if command.endswith('video_thumbnail_smoke'):
                        candidate_checks.append(str(args[1]).rsplit('/', 1)[-1])
                        if failure == 'generated':
                            raise ValueError('synthetic generated smoke failure')
                    return ''

                def build(prefix, sdk):
                    candidate_checks.append('native-build')
                    prefix.mkdir()
                    (prefix / 'synthetic-native-build').touch()

                def stage(executable, prefix, app, sdk):
                    dest = app / 'Contents/MacOS/preview_corpus_smoke'
                    dest.touch()
                    return dest

                def check(app, sdk):
                    candidate_checks.append('inventory')
                    if failure == 'inventory':
                        raise ValueError('synthetic candidate inventory failure')
                    return dict(runtime=[], executables={p.name: m.digest(p) for p in (app / 'Contents/MacOS').iterdir()})

                with patch.object(m, 'ROOT', root), patch.object(m.sys, 'platform', 'darwin'), \
                        patch.object(m, 'run', side_effect=run), patch.object(m, 'build', side_effect=build), \
                        patch.object(m, 'stage_preview', side_effect=stage), patch.object(m, 'check', side_effect=check), \
                        patch.dict(m.os.environ, GITHUB_SHA='synthetic'), patch.dict(m.REDACTIONS):
                    if failure:
                        with self.assertRaises(ValueError if isinstance(failure, str) else failure):
                            m.validate(output)
                    else:
                        m.validate(output)
                self.assertEqual(len(fetches), 1)
                expected_measurements = (0 if unavailable == 'fetch' else
                                         1 if unavailable in ('baseline-discovery', 'baseline-build') else 2)
                self.assertEqual(measurements, fetches * expected_measurements)
                self.assertEqual(len(builds), 2 if unavailable in (None, 'baseline-smoke') else 1)
                self.assertEqual(len({b['CARGO_TARGET_DIR'] for b in builds}), len(builds))
                self.assertTrue(builds[-1]['FFMPEG_DIR'].endswith('/candidate'))
                self.assertEqual(candidate_checks, ['native-build', 'inventory'] +
                                 (['video.avi'] if failure == 'generated' else
                                  [] if failure else ['video.avi', 'still.png']))
                self.assertFalse(app.parent.exists())
                self.assertFalse((root / 'target/macos-runtime-validation').exists())
                report = json.loads(output.read_text())
                self.assertEqual(report['status'], 'failed' if failure else 'passed')
                preview = report['preview']
                if unavailable:
                    self.assertEqual(preview['status'], 'unavailable')
                    self.assertNotIn('baseline', preview)
                    self.assertNotIn('paired_comparisons', preview)
                    self.assertNotIn(str(root), preview['failure'])
                    self.assertIn('synthetic', preview['failure'])
                    self.assertLessEqual(len(preview['failure'].encode()), 32768)
                elif not failure:
                    self.assertEqual(preview['status'], 'passed')
                    self.assertEqual(len(preview['paired_comparisons']), 3)
                if not failure:
                    self.assertEqual(report['smoke']['corpus_correctness'], unavailable != 'fetch')
                self.assertEqual({p.relative_to(root).as_posix() for p in root.rglob('*') if p.is_file()},
                                 {'tests/preview-corpus/fixtures.json', 'audit/candidate.json'})

    def test_corpus_executable_rewrite_is_checked_against_staged_closure(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            executable = root / 'cargo/release/examples/preview_corpus_smoke'
            executable.parent.mkdir(parents=True)
            executable.touch()
            app = root / 'Iris.app'
            for name in ('MacOS', 'Frameworks'):
                (app / 'Contents' / name).mkdir(parents=True)
            lib = root / 'candidate/lib/libavcodec.62.dylib'
            staged = app / 'Contents/Frameworks' / lib.name
            staged.touch()
            dest = app / 'Contents/MacOS/preview_corpus_smoke'
            with patch.object(m, 'libraries', return_value={'avcodec': lib}), \
                    patch.object(m, 'closure', side_effect=[[(executable, str(lib), lib)],
                                                          [(dest, '@executable_path/../Frameworks/' + lib.name, staged.resolve())]]) as closure, \
                    patch.object(m, 'run') as run:
                self.assertEqual(m.stage_preview(executable, root / 'candidate', app, root / 'sdk'), dest)
                closure.assert_called_with(dest, [staged], root / 'sdk')
                self.assertEqual(run.call_args_list[0].args,
                                 ('install_name_tool', '-change', str(lib),
                                  '@executable_path/../Frameworks/' + lib.name, dest))
                self.assertEqual(run.call_args_list[-1].args[-1], app)
            for edges in ([], [(executable, '/usr/lib/libSystem.B.dylib', {'system': True})],
                          [(executable, 'libavformat.dylib', root / 'candidate/lib/libavformat.dylib')]):
                with self.subTest(edges=edges), patch.object(m, 'libraries', return_value={'avcodec': lib}), \
                        patch.object(m, 'closure', return_value=edges), patch.object(m, 'run') as run:
                    with self.assertRaisesRegex(ValueError, 'not linked to candidate libavcodec'):
                        m.stage_preview(executable, root / 'candidate', app, root / 'sdk')
                    run.assert_not_called()
            with patch.object(m, 'libraries', return_value={'avcodec': lib}), \
                    patch.object(m, 'closure', side_effect=[[(executable, str(lib), lib)], []]), \
                    patch.object(m, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'staged preview is not linked'):
                    m.stage_preview(executable, root / 'candidate', app, root / 'sdk')
                self.assertNotIn('codesign', [c.args[0] for c in run.call_args_list])
            with patch.object(m, 'libraries', return_value={'avcodec': lib}), \
                    patch.object(m, 'closure', side_effect=ValueError('outside exact candidate')), \
                    patch.object(m, 'run') as run:
                with self.assertRaises(ValueError):
                    m.stage_preview(executable, root / 'candidate', app, root / 'sdk')
                run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
