"""Bounded Windows runtime input API control; never runs Shells or Agents."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--test-bin', type=Path, required=True)
    parser.add_argument('--fixture', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location('cli_native_control',
        Path(__file__).with_name('cli-native.py'))
    controller = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(controller)
    controller.validate_evidence_directory(args.evidence,
        Path(__file__).resolve().parents[2] / 'docs/local/w1-02')
    test, fixture = args.test_bin.resolve(strict=True), args.fixture.resolve(strict=True)
    stage = args.evidence.resolve() / ('input-api-' + secrets.token_hex(8))
    stage.mkdir(parents=True, exist_ok=False)
    results = []
    for mode in ['bytes', 'wide', 'records']:
        root = stage / mode
        root.mkdir()
        env = {key: value for key, value in os.environ.items()
               if not key.upper().startswith(('HERDR_', 'LOOMTERM_'))}
        env.update(LOOMTERM_ROOT=str(root), LOOMTERM_SESSION='input-control',
                   LOOMTERM_TEST_INPUT_FIXTURE=str(fixture), LOOMTERM_TEST_INPUT_API=mode)
        with (stage / (mode + '.log')).open('wb') as log:
            process = subprocess.Popen([str(test), '--exact',
                'terminal::runtime::windows_input_tests::m2_windows_input_api_control',
                '--ignored', '--nocapture', '--test-threads=1'], cwd=root, env=env,
                stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                creationflags=0x08000000)
            identity = controller.identity(process.pid)
            try:
                process.wait(timeout=40)
            except subprocess.TimeoutExpired:
                # Exact retained Popen handle; fixture itself cancels its read at 15s.
                # Timeout remains a failure, not a successful cleanup assertion.
                process.kill()
                process.wait(timeout=5)
                raise RuntimeError('owned runtime control exceeded 40s watchdog')
        parsed = [json.loads(line[line.index('{'):]) for line in (stage / (mode + '.log')).read_text(
            encoding='utf-8', errors='strict').splitlines() if '{"actual"' in line]
        assert len(parsed) == 1, 'exact runtime observation required; inspect retained failure log'
        result = dict(api=mode, exit_code=process.returncode, test_process=identity,
                      test_process_after=controller.identity(process.pid), observations=parsed)
        results.append(result)
        (stage / 'result.json').write_text(json.dumps(dict(
            scope='Production TerminalRuntime/PTY actor/engine; no Window/TUI/Daemon',
            fixture_sha256=hashlib.sha256(fixture.read_bytes()).hexdigest(),
            test_sha256=hashlib.sha256(test.read_bytes()).hexdigest(), cases=results,
            records_verified=time.time()), ensure_ascii=False, indent=2), encoding='utf-8')
    status = 'Pass' if all(case['exit_code'] == 0 for case in results) else 'Fail'
    print(json.dumps(dict(path=str(stage), status=status, results=results), ensure_ascii=False))
    return 0 if status == 'Pass' else 1


if __name__ == '__main__':
    raise SystemExit(main())
