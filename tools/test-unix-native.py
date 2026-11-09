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
import re
import shutil
import subprocess
import sys
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def lint_configuration(app, env):
    """Require the byte-identical pinned config before any expensive native work."""
    expected = next(f['sha256'] for f in json.loads((app / 'UPSTREAM.json').read_text())['files']
                    if f['path'] == 'clippy.toml')
    config = app / 'clippy.toml'
    actual = digest(config) if config.is_file() else None
    override = env.get('CLIPPY_CONF_DIR')
    if override and Path(override).resolve() != app.resolve():
        raise RuntimeError('Clippy configuration override points outside the production workspace')
    if actual != expected:
        raise RuntimeError('missing or changed pinned upstream clippy.toml in source delivery')
    return dict(present=True, sha256=actual, fixed_upstream_sha256=expected,
                matches_fixed_upstream=True)


def clippy_threshold_probe(app, out, env):
    """Prove the pinned driver reads 11: accept eleven args, reject twelve.

    Uses the same cwd/environment as Cargo, without overriding config discovery.
    Only synthetic source and compiler logs are written, beneath private evidence.
    """
    observations = []
    for count in (11, 12):
        source = out / f'clippy-threshold-{count}.rs'
        source.write_text('pub fn probe(' + ', '.join(f'_a{i}: u8' for i in range(count)) + ') {}\n')
        log = out / f'clippy-threshold-{count}.log'
        argv = ['rustup', 'run', '1.96.1', 'clippy-driver', str(source),
                '--crate-name', 'loomterm_clippy_probe', '--crate-type', 'lib',
                '--emit=metadata', '--out-dir', str(out), '--error-format=json',
                '-D', 'warnings']
        with log.open('wb') as sink:
            result = subprocess.run(argv, cwd=app, env=env, stdout=sink,
                                    stderr=subprocess.STDOUT, timeout=30, check=False)
        if log.stat().st_size > 1024 * 1024:
            raise RuntimeError('Clippy threshold probe output budget exceeded')
        messages = [json.loads(line) for line in log.read_text().splitlines() if line.startswith('{')]
        errors = [m for m in messages if m.get('level') == 'error' and m.get('spans')]
        expected = f'this function has too many arguments ({count}/11)'
        rejected = any((m.get('code') or {}).get('code') == 'clippy::too_many_arguments'
                       and m.get('message') == expected for m in errors)
        if (count == 11 and (result.returncode != 0 or errors)) or (count == 12 and (result.returncode == 0 or not rejected)):
            raise RuntimeError('pinned Clippy did not demonstrate the upstream threshold of 11')
        observations.append(dict(arguments=count, exit_code=result.returncode,
                                 expected_lint_rejection=rejected))
    return dict(status='Pass', threshold=11, observations=observations)


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


def clippy_diagnostics(path, app):
    """Export only lint IDs, workspace locations and numeric argument counts.

    Cargo's bin/test repeats collapse by location and lint. Rendered diagnostics,
    source snippets, arbitrary messages and absolute paths never leave the log.
    """
    report = dict(schema=1, records=[], occurrences=0, rejected=0, truncated=False,
                  complete=True, max_records=128, max_json_bytes=65536)
    records = {}
    app = app.resolve()
    with path.open('rb') as source:
        for line in source:
            if not line.startswith(b'{'):
                continue  # Cargo's own progress text is not a compiler diagnostic.
            try:
                item = json.loads(line)
                if item.get('reason') != 'compiler-message':
                    continue
                message = item['message']
                if message['level'] not in ('warning', 'error'):
                    continue
                report['occurrences'] += 1
                code = (message.get('code') or {}).get('code') or 'rustc::uncoded'
                if not re.fullmatch(r'[A-Za-z0-9_:]{1,96}', code):
                    raise ValueError('noncanonical lint ID')
                span = next(s for s in message['spans'] if s.get('is_primary'))
                raw = Path(span['file_name'])
                candidates = [raw] if raw.is_absolute() else [app / raw, Path(item.get('manifest_path', app / 'Cargo.toml')).parent / raw]
                location = next(p.resolve() for p in candidates
                                if p.resolve().is_relative_to(app) and p.is_file())
                file = location.relative_to(app).as_posix()
                line_number, column = int(span['line_start']), int(span['column_start'])
                if not (0 < line_number < 10_000_000 and 0 < column < 1_000_000):
                    raise ValueError('invalid source position')
                key = (file, line_number, column, code)
                if key in records:
                    records[key]['occurrences'] += 1
                    continue
                record = dict(path=file, line=line_number, column=column, lint=code,
                              end_line=int(span.get('line_end', line_number)),
                              level=message['level'], occurrences=1)
                counts = re.fullmatch(r'this function has too many arguments \((\d+)/(\d+)\)', message.get('message', ''))
                if counts:
                    record.update(actual_args=int(counts[1]), threshold=int(counts[2]))
                symbol = re.search(r'\bfn\s+([A-Za-z_][A-Za-z0-9_]{0,127})',
                                   '\n'.join(s.get('text', '') for s in span.get('text', [])))
                if symbol:
                    record['symbol'] = symbol[1]
                if len(records) == report['max_records']:
                    report['truncated'] = True
                    continue
                report['records'].append(record)
                if len(json.dumps(report).encode()) > report['max_json_bytes']:
                    report['records'].pop()
                    report['truncated'] = True
                    continue
                records[key] = record
            except (ValueError, KeyError, TypeError, StopIteration, OSError):
                report['rejected'] += 1
    report['complete'] = not report['truncated'] and report['rejected'] == 0
    while len(json.dumps(report).encode()) > report['max_json_bytes']:
        report['records'].pop()
        report['truncated'] = True
        report['complete'] = False
    return report


def target():
    system, machine = platform.system(), platform.machine()
    if system == 'Linux' and machine == 'x86_64' and platform.libc_ver()[0] == 'glibc':
        return 'x86_64-unknown-linux-gnu'
    if system == 'Darwin' and machine == 'arm64':
        return 'aarch64-apple-darwin'
    raise RuntimeError(f'requires native Linux x86_64/glibc or macOS arm64; got {system}/{machine}')


def public_runtime_identity(event):
    def select(value, keys):
        return {key: value[key] for key in keys if key in value}
    process_keys = ('pid', 'uid', 'birth', 'precision', 'pgrp', 'sid')
    return dict(event=event['event'], time_ns=event['time_ns'],
                backend=select(event.get('instance', {}), ('product', 'namespace', 'instance_id', 'pid')),
                pane=select(event.get('pane', {}), ('pane_id', 'terminal_id', 'tab_id')),
                shell=select(event.get('shell', {}), process_keys),
                fixture=select(event.get('fixture', {}), process_keys))


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
    config = app / 'clippy.toml'
    config_hash = digest(config) if config.is_file() else None
    results['lint_configuration'] = dict(present=config_hash is not None, sha256=config_hash,
                                        matches_fixed_upstream=False)
    if config_hash:
        results['source_sha256']['apps/loomterm-cli/clippy.toml'] = config_hash

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
        if label == 'clippy':
            results['clippy_diagnostics'] = clippy_diagnostics(log, app)
            if not results['clippy_diagnostics']['complete']:
                raise RuntimeError('Clippy diagnostic export incomplete; inspect private log')
        if code:
            if label == 'clippy':
                # JSON source snippets/rendered text are private too. The old
                # generic tail is only suitable for the other tool phases.
                report = results['clippy_diagnostics']
                results['diagnostic_tail'] = '\n'.join(
                    f"{r['lint']} {r['path']}:{r['line']}:{r['column']}"
                    for r in report['records'])[:8192]
                if not report['records']:
                    report['complete'] = False
                    results['diagnostic_tail'] = 'Clippy failed without structured diagnostics; inspect private log'
            else:
                results['diagnostic_tail'] = diagnostic_tail(log, repo, out)
            raise RuntimeError(f'{label} exited {code}; see private log')
        results['completed_phases'].append(label)
        return log

    try:
        results['active_phase'] = 'lint-configuration'
        results['lint_configuration'] = lint_configuration(app, env)
        results['completed_phases'].append('lint-configuration')
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
        results['active_phase'] = 'clippy-threshold'
        results['lint_configuration']['driver_probe'] = clippy_threshold_probe(app, out, env)
        results['completed_phases'].append('clippy-threshold')
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
        # Engineering acceptance also needs strict Clippy and evidence review.
        results['m2_gate'] = 'Pending'
        results['pending'] = ['abrupt Shell death with live descendants and complete descriptor accounting',
                              'full native slow-client resource load', 'macOS launchd/GUI coalition special branches']
        # Retain runtime evidence even when an independent inherited style lint
        # fails. Clippy still runs, and its failure still fails the whole entry.
        run('clippy', ['cargo', 'clippy', '--workspace', '--all-targets', '--message-format=json', '--target', triple,
                       '--locked', '--', '-D', 'warnings'], 1200)
    except Exception as error:
        results['status'] = 'Fail'
        results['error'] = repr(error)
    finally:
        (out / 'verification.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
    # Only synthetic, selected observations may leave the runner. Never upload
    # .vt, raw logs, HOME, config files, private research or build/cache trees.
    public = dict(status=results['status'], target=triple, m2_gate='Pending',
                  host=dict(system=platform.system(), kernel=platform.release(),
                            architecture=platform.machine(), libc=platform.libc_ver()),
                  source_sha256=results['source_sha256'], artifacts=results.get('artifacts', {}), cases=[],
                  native_headers_sha256=results.get('headers_sha256', {}),
                  link_closure=redact_paths(results.get('closure', ''), repo, out)[:8192],
                  active_phase=results['active_phase'], completed_phases=results['completed_phases'],
                  error=redact_paths(results.get('error', ''), repo, out),
                  diagnostic_tail=results.get('diagnostic_tail', ''),
                  clippy_diagnostics=results.get('clippy_diagnostics'),
                  lint_configuration=results['lint_configuration'],
                  tools={key: redact_paths(value, repo, out) for key, value in results.get('tools', {}).items()})
    for path in sorted(out.glob('*/result.json')):
        case = json.loads(path.read_text())
        timeline_path = path.with_name('timeline.json')
        timeline = json.loads(timeline_path.read_text()) if timeline_path.exists() else []
        public['cases'].append(dict(label=case['label'], status=case['status'], tests=case.get('tests', {}),
                                    phase=case.get('phase', 'unrecorded'),
                                    error=redact_paths(case.get('traceback', '').strip().splitlines()[-1], repo, out)[:512]
                                    if case.get('traceback', '').strip() else '',
                                    cleanup_errors=bool(case.get('cleanup_errors') or case.get('live_registered')),
                                    observations=[e for e in timeline if e['event'] in ('fixture', 'resize', 'wrong_version_rejected')],
                                    identities=[public_runtime_identity(e) for e in timeline if e['event'] in ('instance', 'runtime_target', 'reattached')],
                                    after=[{k: p[k] for k in ('pid', 'uid', 'birth', 'precision', 'exited', 'pgrp', 'sid') if k in p}
                                           for p in case.get('after', [])]))
    (out / 'ci-summary.json').write_text(json.dumps(public, indent=2), encoding='utf-8')
    print(json.dumps(dict(status=results['status'], target=triple, m2_gate='Pending',
                         active_phase=public['active_phase'], error=public['error'],
                         diagnostic_tail=public['diagnostic_tail'])))
    return 0 if results['status'] == 'Pass' else 1


if __name__ == '__main__':
    raise SystemExit(main())
