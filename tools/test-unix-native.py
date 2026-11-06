"""Build and run production native acceptance on a dedicated Linux/macOS runner.

Does not install tools, contact a remote host, or substitute Windows artifacts.
Use --evidence with a new private directory. Do not publish raw .vt/log files.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def redact_paths(text, repo, evidence):
    for path, label in sorted([(repo, '<source>'), (evidence, '<evidence>'),
                               (Path.home(), '<runner-home>')], key=lambda item: len(str(item[0])), reverse=True):
        for spelling in (str(path), path.as_posix()):
            text = text.replace(spelling, label)
    return text


def diagnostic_tail(path, repo, evidence):
    # Only compiler/tool logs, never the native controller's .vt or Shell output.
    with path.open('rb') as source:
        source.seek(0, 2)
        source.seek(max(0, source.tell() - 8192))
        text = source.read(8192).decode('utf-8', errors='replace')
    text = redact_paths(text, repo, evidence)
    return text.encode('utf-8')[-8192:].decode('utf-8', errors='ignore')


def target():
    system, machine = platform.system(), platform.machine()
    if system == 'Linux' and machine == 'x86_64' and platform.libc_ver()[0] == 'glibc':
        return 'x86_64-unknown-linux-gnu'
    if system == 'Darwin' and machine == 'arm64':
        return 'aarch64-apple-darwin'
    raise RuntimeError(f'requires native Linux x86_64/glibc or macOS arm64; got {system}/{machine}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence', type=Path, required=True)
    args = parser.parse_args()
    triple = target()  # refuse non-native hosts before creating output/building
    if os.environ.get('LOOMTERM_NATIVE_MANIFEST'):
        raise RuntimeError('native execution must compile libghostty-vt for this target; unset Windows native manifest')
    repo = Path(__file__).resolve().parents[1]
    app = repo / 'apps/loomterm-cli'
    out = args.evidence.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = os.environ | {'CARGO_TARGET_DIR': str(out / 'target')}
    results = dict(status='Fail', target=triple, uname=list(platform.uname()), libc=platform.libc_ver(),
                   commands=[], completed_phases=[], active_phase='prerequisites',
                   source_sha256={str(p.relative_to(repo)): digest(p) for p in (
                       app / 'Cargo.lock', app / 'UPSTREAM.json', app / 'crates/ghostty-vt/src/bindings.rs',
                       repo / 'tools/fixtures/unix-engine.c', repo / 'tools/fixtures/unix-native.py')})

    def run(label, argv, timeout=120, cwd=app):
        # Streaming into owned files avoids unbounded in-memory compiler output.
        results['active_phase'] = label
        log = out / (label + '.log')
        with log.open('wb') as sink:
            process = subprocess.Popen(argv, cwd=cwd, env=env, stdout=sink, stderr=subprocess.STDOUT)
            results['commands'].append(dict(label=label, argv=argv, pid=process.pid,
                                            executable=shutil.which(argv[0]), created_ns=time.time_ns()))
            try:
                code = process.wait(timeout)
            except subprocess.TimeoutExpired:
                process.terminate()  # exact directly-created tool; no process-name matching
                try:
                    process.wait(10)
                except subprocess.TimeoutExpired:
                    raise RuntimeError(f'{label}: direct tool did not stop; PID {process.pid}; inspect owned runner')
                raise
        if log.stat().st_size > 32 * 1024 * 1024:
            raise RuntimeError(label + ': output budget exceeded')
        if code:
            results['diagnostic_tail'] = diagnostic_tail(log, repo, out)
            raise RuntimeError(f'{label} exited {code}; see private log')
        results['completed_phases'].append(label)
        return log

    try:
        versions = {}
        for tool, argv in [('rustc', ['rustc', '--version', '--verbose']),
                           ('cargo', ['cargo', '--version']), ('zig', ['zig', 'version']),
                           ('cc', ['cc', '--version']), ('linker-command', ['cc', '-###', '-x', 'c', '/dev/null', '-o', str(out / 'linker-probe')])]:
            if not shutil.which(argv[0]):
                raise RuntimeError('missing native prerequisite: ' + argv[0])
            versions[tool] = run('version-' + tool, argv, 20).read_text()
        results['active_phase'] = 'tool-pins'
        assert versions['rustc'].startswith('rustc 1.96.1 '), versions['rustc']
        assert versions['cargo'].startswith('cargo 1.96.1 '), versions['cargo']
        assert versions['zig'].strip() == '0.16.0', versions['zig']
        results['completed_phases'].append('tool-pins')
        if sys.platform == 'darwin':
            versions['sdk'] = run('sdk', ['xcrun', '--show-sdk-path'], 20).read_text().strip()
            versions['sdk_version'] = run('sdk-version', ['xcrun', '--show-sdk-version'], 20).read_text().strip()
            versions['os'] = run('macos-version', ['sw_vers'], 20).read_text()
        else:
            versions['os'] = Path('/etc/os-release').read_text()
            versions['linker'] = run('linker', ['ld', '--version'], 20).read_text()
        results['tools'] = versions
        run('fmt', ['cargo', 'fmt', '--all', '--', '--check'], 120)
        run('metadata', ['cargo', 'metadata', '--locked', '--no-deps', '--format-version=1'], 180)
        run('check', ['cargo', 'check', '--workspace', '--all-targets', '--target', triple, '--locked'], 1200)
        run('clippy', ['cargo', 'clippy', '--workspace', '--all-targets', '--target', triple,
                       '--locked', '--', '-D', 'warnings'], 1200)
        artifacts = run('build', ['cargo', 'test', '--test', 'loomterm_native', '--target', triple,
                                  '--locked', '--no-run', '--message-format=json'], 1200)
        messages = []
        for line in artifacts.read_text().splitlines():
            if line.startswith('{'):
                messages.append(json.loads(line))
        def artifact(name):
            return Path(next(m['executable'] for m in reversed(messages)
                             if m.get('reason') == 'compiler-artifact' and m['target']['name'] == name and m.get('executable')))
        exe, host = artifact('loomterm'), artifact('loomterm_native')
        for prefix in ('m2_', 'm1_', 'backpressure'):
            run('tests-' + prefix.rstrip('_'), ['cargo', 'test', '--bin', 'loomterm', prefix,
                                               '--target', triple, '--locked', '--', '--test-threads=1'], 300)
        run('engine-tests', ['cargo', 'test', '-p', 'ghostty-vt', '--lib', '--target', triple, '--locked'], 300)
        fixture = out / 'unix-engine'
        run('fixture-build', ['cc', '-std=c11', '-Wall', '-Wextra', '-Werror', '-O2',
                               str(repo / 'tools/fixtures/unix-engine.c'), '-o', str(fixture)], 30)
        native = sorted((out / 'target' / triple / 'debug/build').glob('ghostty-vt-*/out/zig-out/lib/*'))
        assert native, 'target-native Ghostty artifact missing'
        results['artifacts'] = {str(p.relative_to(out)): digest(p) for p in [exe, host, fixture, *native] if p.is_file()}
        results['headers_sha256'] = {p.relative_to(app).as_posix(): digest(p)
                                    for p in sorted((app / 'vendor/libghostty-vt/include').rglob('*')) if p.is_file()}
        results['closure'] = run('native-link-closure', ['otool', '-L', str(exe)] if sys.platform == 'darwin'
                                 else ['ldd', str(exe)], 20).read_text()
        # The controller owns its ordinary cleanup. SIGTERM requests it through the
        # Python handler below; no SIGKILL, broad cleanup or runner power operation.
        results['active_phase'] = 'native-controller'
        controller = subprocess.Popen([sys.executable, str(repo / 'tools/fixtures/unix-native.py'),
                                       '--exe', str(exe), '--host', str(host), '--fixture', str(fixture),
                                       '--evidence', str(out)], env=env, cwd=repo)
        try:
            results['controller_exit'] = controller.wait(360)
        except subprocess.TimeoutExpired:
            controller.terminate()
            controller.wait(45)
            raise RuntimeError('native controller deadline; inspect cleanup evidence')
        results['status'] = 'Pass' if results['controller_exit'] == 0 else 'Fail'
        if results['controller_exit'] == 0:
            results['completed_phases'].append('native-controller')
        # A core-case Pass is not the entire M2 Gate: explicit missing assertions remain.
        results['m2_gate'] = 'Pending'
        results['pending'] = ['signal/descendant-held PTY EOF', 'Source Folder change/name',
                              'finite slow-client resource load', 'macOS GUI coalition shutdown']
    except Exception as error:
        results['error'] = repr(error)
    finally:
        (out / 'verification.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
    # Only synthetic, selected observations may leave the runner. Never upload
    # .vt, raw logs, HOME, config files, private research or build/cache trees.
    public = dict(status=results['status'], target=triple, m2_gate='Pending',
                  source_sha256=results['source_sha256'], artifacts=results.get('artifacts', {}), cases=[],
                  active_phase=results['active_phase'], completed_phases=results['completed_phases'],
                  error=redact_paths(results.get('error', ''), repo, out),
                  diagnostic_tail=results.get('diagnostic_tail', ''),
                  tools={key: redact_paths(value, repo, out) for key, value in results.get('tools', {}).items()})
    for path in sorted(out.glob('*/result.json')):
        case = json.loads(path.read_text())
        timeline_path = path.with_name('timeline.json')
        timeline = json.loads(timeline_path.read_text()) if timeline_path.exists() else []
        public['cases'].append(dict(label=case['label'], status=case['status'], tests=case.get('tests', {}),
                                    error=redact_paths(case.get('traceback', '').strip().splitlines()[-1], repo, out)[:512]
                                    if case.get('traceback', '').strip() else '',
                                    cleanup_errors=bool(case.get('cleanup_errors') or case.get('live_registered')),
                                    observations=[e for e in timeline if e['event'] in ('fixture', 'resize', 'wrong_version_rejected')],
                                    after=[{k: p[k] for k in ('pid', 'uid', 'birth', 'precision', 'exited', 'pgrp', 'sid') if k in p}
                                           for p in case.get('after', [])]))
    (out / 'ci-summary.json').write_text(json.dumps(public, indent=2), encoding='utf-8')
    print(json.dumps(dict(status=results['status'], target=triple, m2_gate='Pending',
                         active_phase=public['active_phase'], error=public['error'],
                         diagnostic_tail=public['diagnostic_tail'])))
    return 0 if results['status'] == 'Pass' else 1


if __name__ == '__main__':
    raise SystemExit(main())
