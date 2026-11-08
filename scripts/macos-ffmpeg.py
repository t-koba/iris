#!/usr/bin/env python3
"""Pinned Darwin release runtime and report-only comparison; no ELF mechanics."""
import ctypes
import importlib.util
import json
import os
import signal
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("ffmpeg_policy", ROOT / "scripts/linux-ffmpeg.py")
common = importlib.util.module_from_spec(spec)
spec.loader.exec_module(common)
POLICY, require, digest = common.POLICY, common.require, common.digest
LZMA = json.loads((ROOT / "packaging/macos/liblzma.json").read_text())
MACOS_POLICY = {
    'liblzma': LZMA,
    'platform': 'macOS',
    'source': {key: POLICY[key] for key in (
        'version', 'url', 'sha256', 'signature_url', 'key_url', 'fingerprint',
        'configure_flags', 'libraries', 'avcodec_version', 'security_reference',
        'security_scope', 'required_decoders',
    )},
}


def setting(name, default):
    value = int(os.environ.get(name, default))
    require(value > 0, f"{name} must be positive")
    return value


def clean(text):
    # Neither diagnostics nor retained JSON disclose private build/SDK paths.
    for path, label in sorted(REDACTIONS.items(), key=lambda item: -len(item[0])):
        text = text.replace(path, label)
    return text


REDACTIONS = {str(ROOT): '<source>', str(Path.home()): '<home>'}


def run(*args, cwd=None, env=None, seconds=None):
    with tempfile.TemporaryFile() as log:
        process = subprocess.Popen([str(a) for a in args], cwd=cwd, env=env,
                                   stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = process.wait(timeout=seconds or setting('IRIS_FFMPEG_COMMAND_SECONDS', 60))
        except BaseException as error:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            if isinstance(error, subprocess.TimeoutExpired):
                raise ValueError(f"{Path(args[0]).name} exceeded command deadline") from error
            raise
        size = log.tell()
        limit = setting('IRIS_FFMPEG_COMMAND_BYTES', 32768)
        log.seek(max(0, size - limit))
        text = log.read(limit).decode('utf-8', 'replace')
    require(code == 0, f"{Path(args[0]).name} failed: {clean(text)}")
    # Long successful build logs are discarded. Inspection commands fail on truncation.
    if seconds is None:
        require(size <= limit, f"{Path(args[0]).name} output exceeds command budget")
    return text.strip()


def generated_config(text, components):
    common.config(text)
    for name in ('PIC', 'ZLIB', 'BZLIB', 'LZMA', 'ICONV'):
        require(re.search(rf'^#define CONFIG_{name} 1$', text, re.M), f"missing CONFIG_{name}")
    return common.codecs(components)


def selected_flags(text):
    flags = shlex.split(text)
    selected = [f for f in flags if not f.startswith(('--prefix=', '--libdir='))]
    require(selected == POLICY['configure_flags'], 'unexpected native configure flags')
    return selected


def macho(path):
    """Use load commands, not otool -L (which also prints a dylib's own ID)."""
    deps, rpaths, identity = [], [], None
    for block in run('otool', '-l', path).split('Load command ')[1:]:
        cmd = re.search(r'^\s*cmd (\S+)$', block, re.M)
        require(cmd is not None, 'malformed Mach-O load command')
        cmd = cmd[1]
        if cmd == 'LC_RPATH':
            match = re.search(r'^\s*path (.+) \(offset \d+\)$', block, re.M)
            require(match is not None, 'malformed LC_RPATH')
            rpaths.append(match[1])
        elif cmd.endswith('DYLIB'):
            match = re.search(r'^\s*name (.+) \(offset \d+\)$', block, re.M)
            require(match is not None, 'malformed dylib command')
            if cmd == 'LC_ID_DYLIB':
                require(identity is None, 'multiple dylib IDs')
                identity = match[1]
            else:
                require(cmd in ('LC_LOAD_DYLIB', 'LC_LOAD_WEAK_DYLIB', 'LC_REEXPORT_DYLIB',
                                'LC_LOAD_UPWARD_DYLIB', 'LC_LAZY_LOAD_DYLIB'), 'unknown dylib command')
                deps.append(match[1])
    require(deps, 'no Mach-O dependencies')
    return deps, rpaths, identity


def expand(token, loader, executable):
    for prefix, directory in (('@loader_path', loader.parent), ('@executable_path', executable.parent)):
        if token.startswith(prefix + '/'):
            return Path(os.path.normpath(str(directory / token[len(prefix) + 1:])))
    require(token.startswith('/'), 'unsupported Mach-O path token')
    return Path(os.path.normpath(token))


def system(path, sdk):
    """A system prefix alone is insufficient: require the selected Apple SDK stub.

    Modern macOS keeps many OS dylibs only in the dyld shared cache. SDK .tbd
    evidence verifies OS ownership without misclassifying missing external files.
    The stub filename is not assumed to match the versioned install name
    (for example ``libz.tbd`` provides ``/usr/lib/libz.1.dylib``), so the exact
    install name is matched inside bounded non-recursive stub directories:
    the install-name parent directory plus any enclosing ``.framework`` root.
    System internals are supplied/updated by macOS, not copied into the app.
    """
    name = str(path)
    if not name.startswith(('/usr/lib/', '/System/Library/')):
        return None
    sdk_root = sdk.resolve()
    rel = Path(name.lstrip('/'))
    pending = [sdk_root / rel.parent]
    for index, part in enumerate(rel.parts):
        if part.endswith('.framework'):
            pending.append(sdk_root.joinpath(*rel.parts[:index + 1]))
    seen, unique = set(), []
    for directory in pending:
        try:
            resolved = directory.resolve()
        except Exception:
            raise ValueError('SDK stub escapes SDK')
        require(resolved.is_relative_to(sdk_root), 'SDK stub escapes SDK')
        if resolved not in seen:
            seen.add(resolved)
            unique.append(resolved)
    candidates = []
    for directory in unique:
        if not directory.is_dir():
            continue
        candidates.extend(sorted(directory.glob('*.tbd')))
    stubs = sorted({candidate.resolve() for candidate in candidates})
    matches = []
    for stub in stubs:
        require(stub.is_file() and stub.is_relative_to(sdk_root), 'SDK stub escapes SDK')
        install_names = re.findall(r'''^\s*install-name:\s*['"]?([^'"\s]+)''', stub.read_text(), re.M)
        if name in install_names:
            matches.append(stub)
    # Apple SDKs may include sanitizer variants such as libSystem.B_asan.tbd
    # that advertise the ordinary install name as well. They are not evidence
    # for the ordinary dylib. Prefer the exact basename match after validating
    # its install name; retain ambiguity failures for other duplicate aliases.
    if not rel.stem.endswith('_asan'):
        matches = [stub for stub in matches if not stub.stem.endswith('_asan')]
    require(matches, f'unverified system dependency {json.dumps(name)}: no matching SDK stub')
    canonical_name = rel.name[:-len('.dylib')] + '.tbd' if rel.name.endswith('.dylib') else None
    canonical = (sdk_root / rel.parent / canonical_name).resolve() if canonical_name else None
    if canonical in matches:
        matches = [canonical]
    require(len(matches) == 1,
            f'ambiguous system stub for {json.dumps(name)}: '
            f'{json.dumps([str(stub.relative_to(sdk_root)) for stub in matches])}')
    stub = matches[0]
    return dict(system=name, sdk_stub=str(stub.relative_to(sdk_root)), sdk_stub_sha256=digest(stub))


def closure(executable, allowed, sdk, root=None):
    """Resolve each edge with its loader and inherited dyld run-path stack.

    First existing resolution wins, as with dyld; an external first match is
    rejected rather than skipped in favour of a later acceptable candidate.
    """
    allowed = {p.resolve() for p in allowed}
    edges, names, visited = [], {}, set()
    def walk(loader, inherited, ancestry):
        deps, rpaths, _ = macho(loader)
        stack = tuple(expand(p, loader, executable) for p in rpaths) + inherited
        key = (loader, stack)
        if key in visited:
            return
        visited.add(key)
        require(len(visited) <= setting('IRIS_FFMPEG_MAX_NATIVE_FILES', 256), 'closure exceeds traversal budget')
        for dep in deps:
            require(not re.search(r'(^|/)liblzma[.]', dep), 'liblzma must be statically linked')
            if dep.startswith('@rpath/'):
                options = [p / dep[len('@rpath/'):] for p in stack]
            else:
                options = [expand(dep, loader, executable)]
            found = None
            for option in options:
                option = Path(os.path.normpath(str(option)))
                os_record = system(option, sdk)
                if os_record is not None or option.is_file():
                    found = (option, os_record)
                    break
            require(found is not None, 'unresolved Mach-O edge: ' + clean(dep))
            target, os_record = found
            if os_record is not None:
                edges.append((loader, dep, os_record))
                continue
            target = target.resolve()
            require(target in allowed, 'dependency outside exact candidate/app: ' + clean(str(target)))
            require(target.name not in names or names[target.name] == target, 'dylib basename collision')
            names[target.name] = target
            edges.append((loader, dep, target))
            if target not in ancestry:
                walk(target, stack, ancestry | {target})
    executable = executable.resolve()
    root = root.resolve() if root is not None else executable
    inherited = () if root == executable else tuple(expand(p, executable, executable)
                                                   for p in macho(executable)[1])
    walk(root, inherited, {root})
    return edges


def libraries(prefix):
    libdir = prefix / 'lib'
    result = {}
    for name in POLICY['libraries']:
        path = (libdir / f'lib{name}.dylib').resolve()
        require(path.is_file() and path.parent == libdir.resolve(), 'missing/escaped candidate dylib')
        require(re.fullmatch(rf'lib{name}\.\d+(?:\.\d+)*\.dylib', path.name), 'unexpected dylib identity')
        result[name] = path
    require({p.resolve() for p in libdir.glob('*.dylib')} == set(result.values()), 'unknown candidate dylib')
    require(not list(libdir.glob('*.a')), 'unexpected static candidate library')
    return result


def runtime(paths):
    records = []
    for name, path in paths.items():
        library = ctypes.CDLL(str(path))
        values = {}
        for field in ('configuration', 'license', 'version'):
            fn = getattr(library, f'{name}_{field}')
            fn.restype = ctypes.c_uint if field == 'version' else ctypes.c_char_p
            value = fn()
            values[field] = value if field == 'version' else value.decode()
        values['configuration'] = selected_flags(values['configuration'])
        require(values['license'] == 'LGPL version 2.1 or later', 'unexpected native license')
        value = values['version']
        values['version'] = [value >> 16, (value >> 8) & 255, value & 255]
        if name == 'avcodec':
            require(values['version'] == POLICY['avcodec_version'], 'unexpected avcodec ABI')
        records.append(dict(library=path.name, sha256=digest(path), **values))
    return records


def lzma_signature(text):
    valid = [line.split()[2:] for line in text.splitlines()
             if line.startswith('[GNUPG:] VALIDSIG ')]
    require(len(valid) == 1 and len(valid[0]) in (9, 10)
            and (valid[0][-1] if len(valid[0]) == 10 else valid[0][0]) == LZMA['fingerprint'],
            'liblzma signature is not from the pinned primary key')
    require(not re.search(r'\[GNUPG:\] (BADSIG|ERRSIG|REVKEYSIG|EXPKEYSIG|EXPSIG) ', text),
            'invalid, expired or revoked liblzma signature')
    return dict(verified=True, primary_fingerprint=LZMA['fingerprint'], signing_fingerprint=valid[0][0])


def lzma_evidence(docs):
    require(digest(docs / 'xz-source.tar.xz') == LZMA['sha256'], 'liblzma source hash mismatch')
    license = LZMA['license']
    require(license['scope'] == 'liblzma' and license['spdx'] == '0BSD'
            and license['file'] == 'COPYING.0BSD' and license['metadata_file'] == 'COPYING',
            'unexpected liblzma license metadata')
    require(digest(docs / 'xz-COPYING.0BSD') == license['sha256'], 'liblzma license hash mismatch')
    metadata = docs / 'xz-COPYING'
    require(re.search(r'liblzma.*?BSD\s+Zero\s+Clause\s+License\s+\(0BSD\)', metadata.read_text(), re.S),
            'missing upstream liblzma license declaration')
    return dict(source=LZMA, signature=lzma_signature((docs / 'xz-signature.status').read_text()),
                license_metadata_sha256=digest(metadata))


def build_lzma(work, sdk, env, docs):
    # Use a separate prefix: no static archive or liblzma dylib may enter candidate/lib.
    prefix, source, target = work / 'liblzma', work / f"xz-{LZMA['version']}", work / 'xz-build'
    require(not any(p.exists() for p in (prefix, source, target)), 'liblzma scratch must be fresh')
    for field, filename in (('url', 'xz-source.tar.xz'), ('signature_url', 'xz-source.tar.xz.sig'),
                            ('key_url', 'xz-signing-key.asc')):
        run('curl', '--fail', '--location', '--silent', '--show-error', '--proto', '=https',
            '--proto-redir', '=https', '--tlsv1.2', LZMA[field], '-o', docs / filename,
            seconds=setting('IRIS_FFMPEG_FETCH_SECONDS', 300))
    require(digest(docs / 'xz-source.tar.xz') == LZMA['sha256'], 'liblzma source hash mismatch')
    run('gpg', '--batch', '--import', docs / 'xz-signing-key.asc', env=env)
    keys = run('gpg', '--batch', '--with-colons', '--fingerprint', '--list-keys', LZMA['fingerprint'], env=env)
    require(re.search(r'^pub:[^\n]*\nfpr:::::::::' + LZMA['fingerprint'] + ':$', keys, re.M),
            'missing liblzma primary signing identity')
    status = run('gpg', '--batch', '--status-fd', '1', '--verify', docs / 'xz-source.tar.xz.sig',
                 docs / 'xz-source.tar.xz', env=env)
    lzma_signature(status)
    (docs / 'xz-signature.status').write_text(status + '\n')
    run('tar', '-xJf', docs / 'xz-source.tar.xz', '-C', work)
    for filename in ('COPYING', 'COPYING.0BSD'):
        shutil.copy2(source / filename, docs / ('xz-' + filename))
    evidence = lzma_evidence(docs)
    run('cmake', '-S', source, '-B', target, f'-DCMAKE_INSTALL_PREFIX={prefix}',
        '-DCMAKE_INSTALL_LIBDIR=lib', '-DCMAKE_BUILD_TYPE=Release', f'-DCMAKE_OSX_SYSROOT={sdk}',
        f"-DCMAKE_C_COMPILER={env['CC']}", *LZMA['cmake_flags'], env=env,
        seconds=setting('IRIS_FFMPEG_BUILD_SECONDS', 3600))
    run('cmake', '--build', target, '--config', 'Release', '--parallel', str(setting('IRIS_FFMPEG_JOBS', 2)),
        env=env, seconds=setting('IRIS_FFMPEG_BUILD_SECONDS', 3600))
    run('cmake', '--install', target, '--config', 'Release', '--component', LZMA['install_component'],
        env=env, seconds=setting('IRIS_FFMPEG_BUILD_SECONDS', 3600))
    archive = prefix / 'lib/liblzma.a'
    require(archive.is_file() and (prefix / 'include/lzma.h').is_file()
            and not list(prefix.rglob('*.dylib')), 'missing static-only liblzma installation')
    evidence.update(static_archive_sha256=digest(archive), cmake=run('cmake', '--version').splitlines()[0])
    (docs / 'liblzma-build.json').write_text(json.dumps(evidence, indent=2) + '\n')
    return prefix


def candidate_env(sdk, compiler):
    # Do not let ambient compiler/pkg-config search paths select Homebrew liblzma.
    env = {k: v for k, v in os.environ.items() if k not in (
        'CPATH', 'C_INCLUDE_PATH', 'CPLUS_INCLUDE_PATH', 'OBJC_INCLUDE_PATH', 'LIBRARY_PATH',
        'CPPFLAGS', 'CXXFLAGS', 'LIBS', 'PKG_CONFIG_SYSROOT_DIR', 'PKG_CONFIG',
        'CMAKE_PREFIX_PATH', 'CMAKE_TOOLCHAIN_FILE') and not k.startswith('PKG_CONFIG_')}
    return dict(env, SDKROOT=str(sdk), CC=compiler, CFLAGS=f'-isysroot {shlex.quote(str(sdk))}',
                LDFLAGS=f'-isysroot {shlex.quote(str(sdk))}', PKG_CONFIG_PATH='', PKG_CONFIG_LIBDIR='')


def build(prefix, sdk):
    require(not prefix.exists(), 'candidate prefix must be fresh (no native cache reuse)')
    work = prefix.parent
    env = candidate_env(sdk, run('xcrun', '-f', 'clang'))
    env['GNUPGHOME'] = str(work / 'gnupg')
    Path(env['GNUPGHOME']).mkdir(mode=0o700)
    docs = prefix / 'share/iris-ffmpeg'
    docs.mkdir(parents=True)
    lzma = build_lzma(work, sdk, env, docs)
    env.update(CFLAGS=f'-isysroot {shlex.quote(str(sdk))} -I{shlex.quote(str(lzma / "include"))}',
               LDFLAGS=f'-isysroot {shlex.quote(str(sdk))} -L{shlex.quote(str(lzma / "lib"))} -liconv',
               PKG_CONFIG_PATH=str(lzma / 'lib/pkgconfig'), PKG_CONFIG_LIBDIR=str(lzma / 'lib/pkgconfig'))
    require(run('pkg-config', '--modversion', 'liblzma', env=env) == LZMA['version'], 'wrong liblzma version')
    require(run('pkg-config', '--variable=libdir', 'liblzma', env=env) == str(lzma / 'lib'),
            'liblzma pkg-config escaped prefix')
    # Probe pinned static liblzma and the SDK's other compression/iconv support.
    probe = work / 'probe.c'
    probe.write_text('#include <zlib.h>\n#include <bzlib.h>\n#include <lzma.h>\n#include <iconv.h>\n'
                     'int main(void) { return !zlibVersion() || !BZ2_bzlibVersion() || '
                     '!lzma_version_number() || iconv_open("UTF-8", "UTF-8") == (iconv_t)-1; }\n')
    run(env['CC'], *shlex.split(env['CFLAGS']), probe, '-lz', '-lbz2', lzma / 'lib/liblzma.a',
        '-liconv', '-o', work / 'probe', env=env)
    run(work / 'probe')
    for field, filename in (('url', 'source.tar.xz'), ('signature_url', 'source.tar.xz.asc'),
                            ('key_url', 'signing-key.asc')):
        run('curl', '--fail', '--location', '--silent', '--show-error', '--proto', '=https',
            '--proto-redir', '=https', '--tlsv1.2', POLICY[field], '-o', work / filename,
            seconds=setting('IRIS_FFMPEG_FETCH_SECONDS', 300))
    require(digest(work / 'source.tar.xz') == POLICY['sha256'], 'source hash mismatch')
    run('gpg', '--batch', '--import', work / 'signing-key.asc', env=env)
    keys = run('gpg', '--batch', '--with-colons', '--fingerprint', '--list-keys', POLICY['fingerprint'], env=env)
    require(f"fpr:::::::::{POLICY['fingerprint']}:" in keys.splitlines(), 'missing primary signing identity')
    status = run('gpg', '--batch', '--status-fd', '1', '--verify', work / 'source.tar.xz.asc',
                 work / 'source.tar.xz', env=env)
    common.signature(status)
    (work / 'signature.status').write_text(status + '\n')
    run('tar', '-xJf', work / 'source.tar.xz', '-C', work)
    source = work / f"ffmpeg-{POLICY['version']}"
    run('./configure', f'--prefix={prefix}', f'--libdir={prefix}/lib', *POLICY['configure_flags'],
        cwd=source, env=env, seconds=setting('IRIS_FFMPEG_BUILD_SECONDS', 3600))
    codecs = generated_config((source / 'config.h').read_text(), (source / 'config_components.h').read_text())
    run('make', f"-j{setting('IRIS_FFMPEG_JOBS', 2)}", cwd=source, env=env,
        seconds=setting('IRIS_FFMPEG_BUILD_SECONDS', 3600))
    run('make', 'install', cwd=source, env=env, seconds=setting('IRIS_FFMPEG_BUILD_SECONDS', 3600))
    for filename in ('source.tar.xz', 'source.tar.xz.asc', 'signing-key.asc', 'signature.status'):
        shutil.copy2(work / filename, docs)
    for filename in ('COPYING.LGPLv2.1', 'config.h', 'config_components.h', 'ffbuild/config.mak'):
        (docs / Path(filename).name).write_text(clean((source / filename).read_text()))
    shutil.copy2(ROOT / 'packaging/macos/ffmpeg-README.md', docs / 'README.md')
    shutil.copy2(ROOT / 'scripts/macos-ffmpeg.py', docs / 'macos-ffmpeg.py')
    paths = libraries(prefix)
    for path in paths.values():
        closure(path, paths.values(), sdk)
    manifest = dict(policy=MACOS_POLICY, signature_verified=True, enabled_codecs=codecs,
                    liblzma=json.loads((docs / 'liblzma-build.json').read_text()),
                    libraries={p.name: digest(p) for p in paths.values()},
                    documents={p.name: digest(p) for p in docs.iterdir()},
                    tools=dict(clang=run(env['CC'], '--version').splitlines()[0],
                               sdk=run('xcrun', '--show-sdk-version'), os=run('sw_vers', '-productVersion'),
                               architecture=run('uname', '-m'), rustc=run('rustc', '--version')))
    (docs / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    # Runtime queries happen in a separate process after staging, never mixing two
    # dylibs with identical install names in this Python process.


def stage(prefix, app, sdk):
    paths = libraries(prefix)
    docs = prefix / 'share/iris-ffmpeg'
    manifest = json.loads((docs / 'manifest.json').read_text())
    require(manifest['policy'] == MACOS_POLICY and manifest['signature_verified'] is True, 'candidate policy mismatch')
    require(digest(docs / 'source.tar.xz') == POLICY['sha256'], 'source hash mismatch')
    common.signature((docs / 'signature.status').read_text())
    evidence = lzma_evidence(docs)
    require(all(manifest['liblzma'].get(k) == v for k, v in evidence.items()), 'liblzma evidence mismatch')
    codecs = generated_config((docs / 'config.h').read_text(), (docs / 'config_components.h').read_text())
    require(manifest['enabled_codecs'] == codecs, 'candidate codec manifest mismatch')
    require(manifest['documents'] == {p.name: digest(p) for p in docs.iterdir() if p.name != 'manifest.json'},
            'candidate documents changed')
    require(manifest['libraries'] == {p.name: digest(p) for p in paths.values()}, 'candidate bytes changed')
    frameworks = app / 'Contents/Frameworks'
    require(not list(frameworks.iterdir()), 'stale app frameworks')
    executables = list((app / 'Contents/MacOS').iterdir())
    all_edges = []
    for executable in executables:
        all_edges.extend(closure(executable, paths.values(), sdk))
    for path in paths.values():
        all_edges.extend(closure(app / 'Contents/MacOS/Iris', paths.values(), sdk, root=path))
    for path in paths.values():
        shutil.copy2(path, frameworks / path.name)
        require(digest(frameworks / path.name) == manifest['libraries'][path.name], 'copy identity mismatch')
    # Resolve BEFORE copying/rewriting; loader-relative source contexts are not
    # interchangeable with the destination's context.
    rewrites = {}
    for loader, dep, target in all_edges:
        if isinstance(target, dict):
            continue
        replacement = ('@executable_path/../Frameworks/' if loader in executables else '@loader_path/') + target.name
        key = (loader, dep)
        require(key not in rewrites or rewrites[key] == replacement, 'context-dependent dependency collision')
        rewrites[key] = replacement
    for (loader, dep), replacement in rewrites.items():
        dest = frameworks / loader.name if loader in paths.values() else loader
        run('install_name_tool', '-change', dep, replacement, dest)
    for path in paths.values():
        dest = frameworks / path.name
        run('install_name_tool', '-id', '@loader_path/' + path.name, dest)
        run('codesign', '--force', '--sign', '-', '--timestamp=none', dest)
    for executable in executables:
        if executable.name != 'Iris':
            run('codesign', '--force', '--sign', '-', '--timestamp=none', executable)
    staged_docs = app / 'Contents/Resources/ffmpeg'
    shutil.copytree(docs, staged_docs)
    manifest['staged_libraries'] = {p.name: digest(frameworks / p.name) for p in paths.values()}
    (staged_docs / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')


def check(app, sdk):
    docs = app / 'Contents/Resources/ffmpeg'
    manifest = json.loads((docs / 'manifest.json').read_text())
    require(manifest['policy'] == MACOS_POLICY and manifest['signature_verified'] is True,
            'staged candidate policy mismatch')
    frameworks = app / 'Contents/Frameworks'
    require(set(manifest['staged_libraries']) == set(manifest['libraries'])
            and set(manifest['staged_libraries']) == {p.name for p in frameworks.iterdir()},
            'unexpected staged library set')
    paths = {name: frameworks / path for name, path in
             ((n, next(p for p in manifest['staged_libraries'] if p.startswith(f'lib{n}.'))) for n in POLICY['libraries'])}
    require(manifest['staged_libraries'] == {p.name: digest(p) for p in frameworks.iterdir()}, 'final staged bytes changed')
    for path in paths.values():
        require(macho(path)[2] == '@loader_path/' + path.name, 'unexpected staged dylib ID')
    edges = []
    roots = [(p, None) for p in (app / 'Contents/MacOS').iterdir()]
    roots.extend((app / 'Contents/MacOS/Iris', p) for p in paths.values())
    for executable, root in roots:
        for loader, dep, target in closure(executable, paths.values(), sdk, root=root):
            edges.append(dict(loader=str(loader.relative_to(app)), load_name=dep,
                              target=target if isinstance(target, dict) else str(target.relative_to(app))))
    iris_edges = closure(app / 'Contents/MacOS/Iris', paths.values(), sdk)
    require(any(isinstance(t, Path) and t == paths['avcodec'] for _, _, t in iris_edges), 'Iris is not linked to candidate')
    codecs = generated_config((docs / 'config.h').read_text(), (docs / 'config_components.h').read_text())
    require(manifest['enabled_codecs'] == codecs, 'staged candidate codec manifest mismatch')
    require(manifest['documents'] == {p.name: digest(p) for p in docs.iterdir() if p.name != 'manifest.json'},
            'staged source/notice evidence changed')
    common.signature((docs / 'signature.status').read_text())
    evidence = lzma_evidence(docs)
    require(all(manifest['liblzma'].get(k) == v for k, v in evidence.items()), 'liblzma evidence mismatch')
    require(digest(docs / 'source.tar.xz') == POLICY['sha256'], 'staged source mismatch')
    return dict(source_sha=os.environ.get('GITHUB_SHA', run('git', '-C', ROOT, 'rev-parse', 'HEAD')),
                scope='Pinned macOS runtime inventory; not legal approval, latency parity or release approval.',
                policy=manifest['policy'], signature_verified=True, tools=manifest['tools'], liblzma=manifest['liblzma'],
                enabled_codecs=manifest['enabled_codecs'], documents=manifest['documents'],
                build_libraries=manifest['libraries'], runtime=runtime(paths), closure=edges,
                executables={p.name: digest(p) for p in (app / 'Contents/MacOS').iterdir()})


# Fixed by the smoke's three operations, not an operator scan/count policy.
PREVIEW_OPERATIONS = {
    'image_generic::extract_image_thumbnail': ('landscape.jpg', False),
    'raw::extract_raw_thumbnail': ('canon-eos-r.cr3', False),
    'video::extract_video_thumbnail': ('bunny.mp4', True),
}


def report_bytes(report):
    data = clean(json.dumps(report, indent=2, allow_nan=False)) + '\n'
    require(len(data.encode()) <= setting('IRIS_FFMPEG_MAX_REPORT_BYTES', 266240),
            'report exceeds byte budget')
    return data


def parse_preview(text, source):
    """Accept only complete, bounded smoke records; never retain command logs."""
    require(len(text.encode()) <= setting('IRIS_FFMPEG_COMMAND_BYTES', 32768),
            'preview output exceeds command budget')
    correctness, timings = [], []
    for line in text.splitlines():
        for marker, records in (('PREVIEW_CORRECTNESS ', correctness), ('PREVIEW_LATENCY ', timings)):
            if line.startswith(marker):
                records.append(json.loads(line[len(marker):]))
    require(len(correctness) == 1 and len(timings) == len(PREVIEW_OPERATIONS),
            'missing or duplicate preview records')
    correct = correctness[0]
    require(correct['record'] == 'iris.preview_correctness.v1' and correct['passed'] is True,
            'preview correctness failed')
    identity = correct['identity']
    for key in ('source_revision', 'workflow_run_id', 'cargo_lock_sha256', 'rustc',
                'target_os', 'target_arch', 'runner_name', 'runner_os', 'runner_arch', 'runner_image_os',
                'runner_image_version', 'cpu_model', 'available_parallelism', 'build_profile',
                'release_opt_level', 'crate', 'crate_version'):
        require(identity.get(key) not in (None, ''), f'missing preview identity: {key}')
    require(identity['source_revision'] == source and identity['target_os'] == 'macos'
            and identity['features'] == dict(video=True, pdf=True, raw_full=True)
            and identity['build_profile'] == 'release', 'preview source/features/profile mismatch')
    pins = {p['filename']: p for p in json.loads((ROOT / 'tests/preview-corpus/fixtures.json').read_text())['files']}
    fixtures = correct['fixtures']
    require(len(fixtures) == len(pins) and {f['filename'] for f in fixtures} == set(pins),
            'preview correctness fixture set mismatch')
    for fixture in fixtures:
        pin = pins[fixture['filename']]
        require(fixture['sha256'] == pin['sha256'] and fixture['size_bytes'] == pin['size'],
                'preview fixture pin mismatch')
        validate_preview_output(fixture['output'])
    by_name = {f['filename']: f for f in fixtures}
    require({t['operation'] for t in timings} == set(PREVIEW_OPERATIONS),
            'preview operation set mismatch')
    warmups, samples = int(os.environ['IRIS_PREVIEW_LATENCY_WARMUPS']), int(os.environ['IRIS_PREVIEW_LATENCY_SAMPLES'])
    require(warmups >= 0 and samples > 0, 'invalid preview iteration budget')
    for timing in timings:
        name, uses_ffmpeg = PREVIEW_OPERATIONS[timing['operation']]
        fixture = by_name[name]
        require(timing['record'] == 'iris.preview_latency.v1' and timing['identity'] == identity
                and timing['uses_ffmpeg'] is uses_ffmpeg, 'preview timing identity mismatch')
        require(timing['input'] == dict(filename=name, sha256=fixture['sha256'], size_bytes=fixture['size_bytes']),
                'preview timing fixture mismatch')
        require({k: timing['output'][k] for k in fixture['output']} == fixture['output'],
                'preview timing output mismatch')
        stats = timing['timing']
        require(stats['unit'] == 'nanoseconds' and stats['warmup_iterations'] == warmups
                and stats['sample_count'] == samples, 'preview timing budget mismatch')
        values = [stats[k] for k in ('min', 'median', 'p90', 'max', 'median_absolute_deviation')]
        require(all(type(v) is int and 0 <= v <= (1 << 64) - 1 for v in values)
                and values[:4] == sorted(values[:4]), 'invalid preview timing statistics')
    return dict(correctness=correct, timings=timings)


def validate_preview_output(output):
    require(output['format'] == 'jpeg' and isinstance(output['source'], str) and output['source']
            and all(type(output[k]) is int and 0 < output[k] <= 256 for k in ('width', 'height')),
            'invalid preview output dimensions/format/source')


def paired_preview(baseline, candidate):
    """Fail closed instead of comparing measurements from differing experiments."""
    left, right = baseline['correctness'], candidate['correctness']
    require(left['identity'] == right['identity'], 'paired preview identity mismatch')
    require(sorted(left['fixtures'], key=lambda f: f['filename']) ==
            sorted(right['fixtures'], key=lambda f: f['filename']), 'paired preview correctness mismatch')
    left = {t['operation']: t for t in baseline['timings']}
    right = {t['operation']: t for t in candidate['timings']}
    require(set(left) == set(right) == set(PREVIEW_OPERATIONS), 'paired preview operations mismatch')
    pairs = []
    for operation, baseline_record in left.items():
        candidate_record = right[operation]
        for key in ('input', 'identity', 'uses_ffmpeg'):
            require(baseline_record[key] == candidate_record[key], f'paired preview {key} mismatch')
        for key in ('format', 'width', 'height', 'source'):
            require(baseline_record['output'][key] == candidate_record['output'][key],
                    'paired preview output mismatch')
        for key in ('unit', 'warmup_iterations', 'sample_count'):
            require(baseline_record['timing'][key] == candidate_record['timing'][key],
                    'paired preview timing budget mismatch')
        b, c = baseline_record['timing']['median'], candidate_record['timing']['median']
        pairs.append(dict(operation=operation, input=baseline_record['input'],
                          uses_ffmpeg=baseline_record['uses_ffmpeg'],
                          output_dimensions=[baseline_record['output'][k] for k in ('width', 'height')],
                          baseline_median_ns=b, candidate_median_ns=c, candidate_minus_baseline_ns=c-b))
    return pairs


def preview_env(prefix, target, source):
    require(not target.exists(), 'preview Cargo target must be fresh')
    # Same build settings, but no ambient native discovery overrides for either runtime.
    env = {k: v for k, v in os.environ.items() if not k.startswith(('FFMPEG_', 'PKG_CONFIG_', 'IRIS_MACOS_FFMPEG_'))
           and k not in ('CPATH', 'C_INCLUDE_PATH', 'CPLUS_INCLUDE_PATH', 'OBJC_INCLUDE_PATH', 'LIBRARY_PATH',
                        'CFLAGS', 'CXXFLAGS', 'CPPFLAGS', 'LDFLAGS', 'LIBS', 'PKG_CONFIG',
                        'CMAKE_PREFIX_PATH', 'CMAKE_TOOLCHAIN_FILE')}
    return dict(env, FFMPEG_DIR=str(prefix), PKG_CONFIG_PATH=str(prefix / 'lib/pkgconfig'),
                PKG_CONFIG_LIBDIR=str(prefix / 'lib/pkgconfig'), CARGO_TARGET_DIR=str(target), GITHUB_SHA=source)


def build_preview(env):
    prefix = Path(env['FFMPEG_DIR']).resolve()
    require(Path(run('pkg-config', '--variable=prefix', 'libavcodec', env=env)).resolve() == prefix,
            'preview pkg-config selected another runtime')
    run('cargo', 'build', '--release', '-p', 'iris-thumb-preview', '--example', 'preview_corpus_smoke',
        '--locked', '--offline', cwd=ROOT, env=env, seconds=setting('IRIS_FFMPEG_BUILD_SECONDS', 3600))
    return Path(env['CARGO_TARGET_DIR']) / 'release/examples/preview_corpus_smoke'


def stage_preview(executable, prefix, app, sdk):
    paths = libraries(prefix)
    edges = closure(executable, paths.values(), sdk)
    require(any(t == paths['avcodec'] for _, _, t in edges), 'preview is not linked to candidate libavcodec')
    dest = app / 'Contents/MacOS/preview_corpus_smoke'
    shutil.copy2(executable, dest)
    for loader, dep, target in edges:
        if loader == executable and isinstance(target, Path):
            run('install_name_tool', '-change', dep, '@executable_path/../Frameworks/' + target.name, dest)
    staged = list((app / 'Contents/Frameworks').iterdir())
    edges = closure(dest, staged, sdk)  # Reject a remaining prefix/Homebrew edge before execution.
    require(any(t == (app / 'Contents/Frameworks' / paths['avcodec'].name).resolve() for _, _, t in edges),
            'staged preview is not linked to candidate libavcodec')
    run('codesign', '--force', '--sign', '-', '--timestamp=none', dest)
    run('codesign', '--force', '--sign', '-', '--timestamp=none', app)
    return dest


def sign_runtime(app, identity):
    """Only rebind hashes across our own signing of the already-verified bytes."""
    docs = app / 'Contents/Resources/ffmpeg'
    manifest = json.loads((docs / 'manifest.json').read_text())
    frameworks = app / 'Contents/Frameworks'
    require(manifest['staged_libraries'] == {p.name: digest(p) for p in frameworks.iterdir()},
            'runtime changed before signing')
    for name in manifest['staged_libraries']:
        path = frameworks / name
        args = ['codesign', '--force', '--sign', identity, '--timestamp=none']
        if identity != '-':
            args += ['--options', 'runtime']
        run(*args, path)
        run('codesign', '--verify', '--strict', path)
        manifest['staged_libraries'][name] = digest(path)
    (docs / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')


def product(app, sdk):
    """Build, stage and exercise the release runtime; no Homebrew fallback."""
    require(sys.platform == 'darwin', 'native macOS required')
    require(not any(k.startswith('DYLD_') for k in os.environ), 'ambient DYLD overrides forbidden')
    require(not os.environ.get('IRIS_MACOS_REPORT_ONLY'), 'product cannot be report-only')
    # Native and Cargo discovery inputs are never reused from another runtime.
    scratch = ROOT / 'target'
    scratch.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='macos-product-', dir=scratch) as temporary:
        work = Path(temporary).resolve()
        REDACTIONS[str(work)] = '<temporary>'
        REDACTIONS[str(sdk)] = '<sdk>'
        prefix = work / 'runtime'
        build(prefix, sdk)
        source = os.environ.get('GITHUB_SHA') or run('git', '-C', ROOT, 'rev-parse', 'HEAD')
        env = preview_env(prefix, work / 'cargo', source)
        preview = build_preview(env)
        run('cargo', 'build', '--locked', '--offline', '--release', '-p', 'iris-ui-shell', '--bin', 'Iris',
            cwd=ROOT, env=env, seconds=setting('IRIS_FFMPEG_BUILD_SECONDS', 3600))
        macos = app / 'Contents/MacOS'
        shutil.copy2(Path(env['CARGO_TARGET_DIR']) / 'release/Iris', macos / 'Iris')
        smoke = macos / 'preview_corpus_smoke'
        shutil.copy2(preview, smoke)
        stage(prefix, app, sdk)
        staged = list((app / 'Contents/Frameworks').iterdir())
        for executable in (macos / 'Iris', smoke):
            edges = closure(executable, staged, sdk)
            require(any(isinstance(t, Path) and t.name.startswith('libavcodec.') for _, _, t in edges),
                    'product executable is not linked to staged libavcodec')
        corpus = work / 'corpus'
        run(sys.executable, ROOT / 'scripts/fetch-preview-corpus.py', '--output', corpus, cwd=ROOT,
            seconds=setting('IRIS_FFMPEG_FETCH_SECONDS', 300))
        # This is mandatory correctness, not the optional paired latency experiment.
        correctness = parse_preview(run(smoke, corpus, env=env), source)
        smoke.unlink()  # Test tooling is not part of the distributed product.
        sign_runtime(app, os.environ.get('IRIS_MACOS_SIGN_IDENTITY', '-'))
        report = check(app, sdk)
        # The outer app signature is applied later. Do not label pre-sign main
        # executable hashes as final artifact bytes; nested dylib hashes are final.
        del report['executables']
        report['preview'] = correctness
        (app / 'Contents/Resources/ffmpeg-runtime.json').write_text(report_bytes(report))


def validate(output):
    require(sys.platform == 'darwin', 'native macOS required')
    require(not any(k.startswith('DYLD_') for k in os.environ), 'ambient DYLD overrides forbidden')
    app = ROOT / 'dist/macos/Iris.app'
    require(not app.parent.exists(), 'temporary macOS output must be fresh')
    report = dict(scope='Report-only pinned candidate (not the current release runtime); not legal approval or release approval.', status='failed')
    scratch = ROOT / 'target/macos-runtime-validation'
    require(not scratch.exists(), 'candidate scratch must be fresh')
    scratch.mkdir(parents=True)
    try:
        with tempfile.TemporaryDirectory(prefix='native-', dir=scratch) as temporary:
            work = Path(temporary).resolve()
            REDACTIONS[str(work)] = '<temporary>'
            sdk = Path(run('xcrun', '--sdk', 'macosx', '--show-sdk-path')).resolve()
            REDACTIONS[str(sdk)] = '<sdk>'
            source = os.environ.get('GITHUB_SHA') or run('git', '-C', ROOT, 'rev-parse', 'HEAD')
            corpus = work / 'corpus'
            preview = dict(scope='Paired sequential crate-level diagnostics, no threshold, parity claim or legal approval.',
                           baseline_runtime='Existing Homebrew FFmpeg selection (not a compatible-runtime approval)',
                           candidate_runtime='Pinned prefix, smoke executed against staged native closure', status='unavailable')
            report['preview'] = preview
            corpus_available = False
            step = 'corpus fetch'
            try:
                # One fetch, one caller-owned directory, reused unchanged for both runtimes.
                run(sys.executable, ROOT / 'scripts/fetch-preview-corpus.py', '--output', corpus, cwd=ROOT,
                    seconds=setting('IRIS_FFMPEG_FETCH_SECONDS', 300))
                corpus_available = True
                step = 'Homebrew baseline'
                baseline_prefix = Path(run('brew', '--prefix', 'ffmpeg')).resolve()
                baseline_env = preview_env(baseline_prefix, work / 'cargo-baseline', source)
                baseline_exe = build_preview(baseline_env)
                preview['baseline'] = parse_preview(run(baseline_exe, corpus, env=baseline_env), source)
            except InterruptedError:
                raise
            except Exception as error:
                # Optional comparison cannot suppress candidate inventory/generated smokes.
                preview['failure'] = (step + ': ' + clean(str(error))).encode()[:
                    setting('IRIS_FFMPEG_COMMAND_BYTES', 32768)].decode('utf-8', 'ignore')
            prefix, target = work / 'candidate', work / 'cargo-candidate'
            build(prefix, sdk)
            env = dict(preview_env(prefix, target, source), IRIS_MACOS_REPORT_ONLY='1', IRIS_MACOS_FFMPEG_CANDIDATE=str(prefix),
                       IRIS_BUILD_VARIANT='product', IRIS_MACOS_SIGN_IDENTITY='-', IRIS_MACOS_NOTARY_PROFILE='')
            candidate_exe = build_preview(env)
            run('cargo', 'build', '--release', '-p', 'iris-thumb-preview', '--example', 'video_thumbnail_smoke',
                '--locked', '--offline', cwd=ROOT, env=env, seconds=setting('IRIS_FFMPEG_BUILD_SECONDS', 3600))
            run(ROOT / 'scripts/package-macos-app.sh', cwd=ROOT, env=env,
                seconds=setting('IRIS_FFMPEG_BUILD_SECONDS', 3600))
            candidate_smoke = stage_preview(candidate_exe, prefix, app, sdk)
            run('codesign', '--verify', '--deep', '--strict', app)
            report.update(check(app, sdk), scope=report['scope'])
            if corpus_available:
                preview['candidate'] = parse_preview(run(candidate_smoke, corpus, env=env), source)
                if 'baseline' in preview:
                    preview['paired_comparisons'] = paired_preview(preview['baseline'], preview['candidate'])
                    preview['status'] = 'passed'
            # Retain the existing additional MPEG-4/PNG candidate checks.
            for filename, options in (('video.avi', ['-t', '1', '-c:v', 'mpeg4', '-an']),
                                      ('still.png', ['-frames:v', '1', '-c:v', 'png'])):
                run(prefix / 'bin/ffmpeg', '-nostdin', '-v', 'error', '-f', 'lavfi', '-i',
                    'testsrc2=size=64x48:rate=4', *options, work / filename, env=env)
            smoke = app / 'Contents/MacOS/video_thumbnail_smoke'
            run(smoke, work / 'video.avi', env=env)
            run(smoke, work / 'still.png', '--still', env=env)
            require(report['executables'] == {p.name: digest(p) for p in (app / 'Contents/MacOS').iterdir()},
                    'executable bytes changed during smoke')
            require({r['library']: r['sha256'] for r in report['runtime']} ==
                    {p.name: digest(p) for p in (app / 'Contents/Frameworks').iterdir()},
                    'library bytes changed during smoke')
            report.update(status='passed', smoke=dict(video_mpeg4=True, still_png_file_and_bytes=True,
                          corpus_correctness=corpus_available, raw_scope='Embedded-preview extraction, not sensor demosaic.'))
    except BaseException as error:
        report['status'] = 'failed'
        report['failure'] = clean(str(error))
        raise
    finally:
        shutil.rmtree(app.parent, ignore_errors=True)
        shutil.rmtree(scratch, ignore_errors=True)
        data = report_bytes(report)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(data)


if __name__ == '__main__':
    try:
        if sys.argv[1] in ('validate', 'product'):
            # GitHub cancellation sends TERM before KILL. Unwind owned scratch
            # and active process groups; the workflow's always cleanup is a backstop.
            def interrupted(signum, frame):
                raise InterruptedError('runtime validation interrupted')
            signal.signal(signal.SIGTERM, interrupted)
        if sys.argv[1] == 'validate' and len(sys.argv) == 3:
            validate(Path(sys.argv[2]))
        elif sys.argv[1] in ('product', 'check') and len(sys.argv) == 3:
            sdk = Path(run('xcrun', '--sdk', 'macosx', '--show-sdk-path')).resolve()
            REDACTIONS[str(sdk)] = '<sdk>'
            (product if sys.argv[1] == 'product' else check)(Path(sys.argv[2]).resolve(), sdk)
        elif sys.argv[1] == 'stage' and len(sys.argv) == 4:
            stage(Path(sys.argv[2]).resolve(), Path(sys.argv[3]).resolve(),
                  Path(run('xcrun', '--sdk', 'macosx', '--show-sdk-path')).resolve())
        else:
            raise ValueError('expected validate OUTPUT, stage PREFIX APP, product APP or check APP')
    except Exception as error:
        sys.exit(clean(str(error)))
