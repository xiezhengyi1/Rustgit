#!/usr/bin/env python3
"""Compare Git / RustGit CLI wall time on identical disposable datasets.

No production repositories or credentials are used. TCP servers bind loopback only.
Python 3.9+; Git; a release rgit binary. Results include all individual samples.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import shutil
import socket
import statistics
import subprocess
import tempfile
import time

OPERATIONS = {
    'init': ['init -q -b main', 'init'],
    'commit_initial': ['add -A; commit -q -m initial', 'commit -m initial'],
    'status_clean': ['status --porcelain=v1', 'status'],
    'status_dirty': ['status --porcelain=v1', 'status'],
    'diff': ['diff --no-ext-diff', 'diff'],
    'log': ['log -n 100 --format=medium', 'log -n 100'],
    'show': ['show --no-ext-diff HEAD', 'show HEAD'],
    'commit_incremental': ['add -A; commit -q -m incremental', 'commit -m incremental'],
    'branch': ['branch feature', 'branch feature'],
    'switch_old': ['switch -q first', 'switch first'],
    'switch_back': ['switch -q main', 'switch main'],
    'restore_file': ['restore --source HEAD~1 -- notes.txt', 'restore --from HEAD~1 notes.txt'],
    'shelve': ['stash push -q -m wip', 'shelve -m wip'],
    'unshelve': ['stash pop -q', 'unshelve 1'],
    'revert': ['revert --no-edit HEAD', 'revert HEAD'],
    'merge': ['merge -q --no-edit --no-ff feature', 'merge feature'],
    'push_initial': ['push -q origin main', 'push'],
    'clone': ['clone -q git://127.0.0.1:PORT/host.git COPY', 'clone tcp://127.0.0.1:PORT COPY --host-key FINGERPRINT'],
    'push_incremental': ['push -q origin main', 'push'],
    'fetch_incremental': ['fetch -q origin', 'fetch'],
    'pull_incremental': ['pull -q --no-rebase origin main', 'pull'],
    'push_noop': ['push -q origin main', 'push'],
    'fetch_noop': ['fetch -q origin', 'fetch'],
    'pull_noop': ['pull -q --no-rebase origin main', 'pull'],
    'verify': ['fsck --full', 'verify'],
    'gc': ['gc --quiet', 'gc'],
}


def capture(cmd):
    return subprocess.check_output(cmd, text=True).strip()


def tree_digest(root):
    digest = hashlib.sha256()
    count = size = 0
    for base, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d not in ('.git', '.rgit'))
        for name in sorted(files):
            p = Path(base) / name
            data = p.read_bytes()
            digest.update(p.relative_to(root).as_posix().encode() + b'\0')
            digest.update(hashlib.sha256(data).digest())
            count += 1
            size += len(data)
    return {'files': count, 'bytes': size, 'sha256': digest.hexdigest()}


def dataset(root, name, seed):
    rng = random.Random(seed)
    if name == 'source':
        for i in range(300):
            p = root / f'src/module{i // 30:02}/file{i:04}.txt'
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_bytes(b''.join((f'const value_{j} = "{rng.getrandbits(64):016x}";\n'.encode()) for j in range(200)))
    elif name == 'tiny':
        for i in range(5000):
            p = root / f'items/group{i // 100:02}/item{i:05}.txt'
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_bytes(rng.randbytes(256).hex().encode())
    elif name == 'large':
        for i in range(4):
            (root / f'data{i}.bin').write_bytes(rng.randbytes(16 * 1024 * 1024))
    (root / 'notes.txt').write_text('version 0\n', encoding='utf-8')


def edit(root, name, step):
    (root / 'notes.txt').write_text(f'version {step}\n', encoding='utf-8')
    candidates = sorted(root.glob('src/*/*.txt') if name == 'source' else
                        root.glob('items/*/*.txt') if name == 'tiny' else root.glob('data*.bin'))
    for i, p in enumerate(candidates[:5]):
        with p.open('r+b') as stream:
            stream.seek((step * 97 + i * 13) % max(1, p.stat().st_size - 64))
            stream.write(hashlib.sha256(f'{step}-{i}'.encode()).hexdigest().encode())


class Trial:
    def __init__(self, tool, root, binary, dataset_name):
        self.tool, self.root, self.binary, self.dataset_name = tool, root, binary, dataset_name
        self.env = dict(os.environ)
        for k in list(self.env):
            if k.startswith('GIT_') or k.startswith('RGIT_'):
                del self.env[k]
        self.env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull,
                        GIT_TERMINAL_PROMPT='0', GIT_AUTHOR_DATE='2026-01-01T00:00:00Z',
                        GIT_COMMITTER_DATE='2026-01-01T00:00:00Z',
                        RGIT_HOME=str(root / 'identity'), RGIT_INSECURE_FILE_KEYS='1')
        self.results, self.checks, self.servers = {}, {}, []

    def cmd(self, cwd, *args, output=False):
        cmd = (['git', '-C', str(cwd)] if self.tool == 'git' else
               [self.binary, '-C', str(cwd)]) + list(map(str, args))
        proc = subprocess.run(cmd, env=self.env, stdout=subprocess.PIPE if output else subprocess.DEVNULL,
                              stderr=subprocess.PIPE, timeout=180)
        if proc.returncode:
            raise RuntimeError(f'{self.tool} {args[0]} failed ({proc.returncode}): {proc.stderr.decode(errors="replace")[-2000:]}')
        return proc.stdout.decode() if output else None

    def timed(self, name, fn):
        start = time.perf_counter_ns()
        result = fn()
        self.results[name] = (time.perf_counter_ns() - start) / 1e6
        return result

    def config(self, cwd):
        settings = {'user.name': 'Benchmark', 'user.email': 'bench@example.com'}
        if self.tool == 'git':
            settings.update({'gc.auto': '0', 'gc.autoDetach': 'false', 'maintenance.auto': 'false',
                             'commit.gpgsign': 'false', 'core.autocrlf': 'false', 'core.fsmonitor': 'false',
                             'core.hooksPath': os.devnull})
        for key, value in settings.items():
            self.cmd(cwd, 'config', key, value)

    def commit(self, cwd, message):
        if self.tool == 'git':
            self.cmd(cwd, 'add', '-A')
            self.cmd(cwd, 'commit', '-q', '-m', message)
        else:
            self.cmd(cwd, 'commit', '-m', message)

    def switch(self, cwd, branch):
        self.cmd(cwd, 'switch', branch)

    def invite(self, host, address):
        raw = self.cmd(host, '--json', 'invite', 'create', '--role', 'write', '--expires', '1h', '--hint', address, output=True)
        def uri(obj):
            if isinstance(obj, str) and obj.startswith('projectapp://join/'):
                return obj
            if isinstance(obj, dict):
                for value in obj.values():
                    found = uri(value)
                    if found:
                        return found
        found = uri(json.loads(raw.splitlines()[-1]))
        if not found:
            raise RuntimeError('Invitation missing from result')
        return found

    def network(self, work):
        host = self.root / ('host.git' if self.tool == 'git' else 'host')
        if self.tool == 'git':
            host.mkdir()
            self.cmd(host, 'init', '--bare', '-q', '-b', 'main')
            self.config(host)
            with socket.socket() as sock:
                sock.bind(('127.0.0.1', 0))
                port = sock.getsockname()[1]
            server = subprocess.Popen(['git', 'daemon', '--reuseaddr', '--export-all', '--enable=receive-pack',
                                       '--listen=127.0.0.1', f'--port={port}', f'--base-path={self.root}', str(self.root)],
                                      env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            self.servers.append(server)
            for _ in range(100):
                if server.poll() is not None:
                    raise RuntimeError('Git daemon exited')
                try:
                    with socket.create_connection(('127.0.0.1', port), timeout=.1):
                        break
                except OSError:
                    time.sleep(.02)
            url = f'git://127.0.0.1:{port}/host.git'
            self.cmd(work, 'remote', 'add', 'origin', url)
        else:
            port_file = self.root / 'port'
            server = subprocess.Popen([self.binary, 'serve', '--init', str(host), '--listen', '127.0.0.1:0', '--port-file', str(port_file)],
                                      env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            self.servers.append(server)
            for _ in range(100):
                if server.poll() is not None:
                    raise RuntimeError('RustGit host exited')
                if port_file.exists() and port_file.read_text().strip():
                    break
                time.sleep(.02)
            url = f'tcp://{port_file.read_text().strip()}'
            self.cmd(work, 'join', self.invite(host, url), '--add-remote')
        push = lambda: self.cmd(work, 'push', *(['-q', 'origin', 'main'] if self.tool == 'git' else []))
        self.timed('push_initial', push)
        copy = self.root / 'copy'
        host_key = self.cmd(work, 'config', 'remote.origin.host_key', output=True).strip() if self.tool == 'rgit' else None
        clone = lambda dest: self.cmd(self.root, 'clone', *(['-q'] if self.tool == 'git' else []), url, dest, *(['--host-key', host_key] if host_key else []))
        self.timed('clone', lambda: clone(copy))
        baseline = tree_digest(work)
        assert tree_digest(copy) == baseline, 'Full clone content mismatch'
        self.checks['clone'] = baseline
        pull_copy = self.root / 'pull-copy'
        clone(pull_copy)
        edit(work, self.dataset_name, 20)
        self.commit(work, 'network increment')
        self.timed('push_incremental', push)
        fetch = lambda: self.cmd(copy, 'fetch', *(['-q', 'origin'] if self.tool == 'git' else []))
        self.timed('fetch_incremental', fetch)
        assert tree_digest(copy) == baseline, 'Fetch changed working files'
        pull = lambda: self.cmd(pull_copy, 'pull', *(['-q', '--no-rebase', 'origin', 'main'] if self.tool == 'git' else []))
        self.timed('pull_incremental', pull)
        updated = tree_digest(work)
        assert tree_digest(pull_copy) == updated, 'Pull content mismatch'
        self.checks['pull'] = updated
        self.timed('push_noop', push)
        self.timed('fetch_noop', fetch)
        self.timed('pull_noop', pull)

    def run(self, template):
        self.root.mkdir()
        work = self.root / 'work'
        work.mkdir()
        self.timed('init', lambda: self.cmd(work, 'init', *(['-q', '-b', 'main'] if self.tool == 'git' else [])))
        self.config(work)
        shutil.copytree(template, work, dirs_exist_ok=True)
        self.timed('commit_initial', lambda: self.commit(work, 'initial'))
        self.cmd(work, 'branch', 'first')
        for step in range(1, 11):
            edit(work, self.dataset_name, step)
            self.commit(work, f'history {step}')
        status = lambda: self.cmd(work, 'status', *(['--porcelain=v1'] if self.tool == 'git' else []))
        # One untimed status scan primes stat caches for the clean/dirty status measurements.
        status()
        self.timed('status_clean', status)
        edit(work, self.dataset_name, 11)
        self.timed('status_dirty', status)
        self.timed('diff', lambda: self.cmd(work, 'diff', *(['--no-ext-diff'] if self.tool == 'git' else [])))
        self.timed('log', lambda: self.cmd(work, 'log', '-n', '100', *(['--format=medium'] if self.tool == 'git' else [])))
        self.timed('show', lambda: self.cmd(work, 'show', *(['--no-ext-diff'] if self.tool == 'git' else []), 'HEAD'))
        self.timed('commit_incremental', lambda: self.commit(work, 'incremental'))
        self.timed('branch', lambda: self.cmd(work, 'branch', 'feature'))
        self.timed('switch_old', lambda: self.switch(work, 'first'))
        self.checks['oldest'] = tree_digest(work)
        assert self.checks['oldest'] == tree_digest(template), 'Oldest checkout mismatch'
        self.timed('switch_back', lambda: self.switch(work, 'main'))
        self.timed('restore_file', lambda: self.cmd(work, 'restore', *(['--source', 'HEAD~1', '--'] if self.tool == 'git' else ['--from', 'HEAD~1']), 'notes.txt'))
        assert (work / 'notes.txt').read_text() == 'version 10\n'
        self.cmd(work, 'restore', *(['--source', 'HEAD', '--'] if self.tool == 'git' else ['--from', 'HEAD', '--force']), 'notes.txt')
        (work / 'notes.txt').write_text('work in progress\n')
        self.timed('shelve', lambda: self.cmd(work, *(['stash', 'push', '-q', '-m', 'wip'] if self.tool == 'git' else ['shelve', '-m', 'wip'])))
        assert (work / 'notes.txt').read_text() == 'version 11\n'
        self.timed('unshelve', lambda: self.cmd(work, *(['stash', 'pop', '-q'] if self.tool == 'git' else ['unshelve', '1'])))
        assert (work / 'notes.txt').read_text() == 'work in progress\n'
        self.cmd(work, 'restore', *(['--source', 'HEAD', '--'] if self.tool == 'git' else ['--from', 'HEAD', '--force']), 'notes.txt')
        self.timed('revert', lambda: self.cmd(work, 'revert', *(['--no-edit'] if self.tool == 'git' else []), 'HEAD'))
        assert (work / 'notes.txt').read_text() == 'version 10\n'
        self.switch(work, 'feature')
        (work / 'feature.txt').write_text('topic change\n')
        self.commit(work, 'topic change')
        self.switch(work, 'main')
        (work / 'main.txt').write_text('main change\n')
        self.commit(work, 'main change')
        self.timed('merge', lambda: self.cmd(work, 'merge', *(['-q', '--no-edit', '--no-ff'] if self.tool == 'git' else []), 'feature'))
        assert (work / 'feature.txt').read_text() == 'topic change\n'
        assert (work / 'main.txt').read_text() == 'main change\n'
        self.checks['merged'] = tree_digest(work)
        self.network(work)
        self.timed('verify', lambda: self.cmd(work, *(['fsck', '--full'] if self.tool == 'git' else ['verify'])))
        self.timed('gc', lambda: self.cmd(work, 'gc', *(['--quiet'] if self.tool == 'git' else [])))
        self.cmd(work, *(['fsck', '--full'] if self.tool == 'git' else ['verify']))
        return {'milliseconds': self.results, 'content_checks': self.checks}

    def close(self):
        for proc in reversed(self.servers):
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rgit', type=Path, default=Path('target/release/rgit'))
    parser.add_argument('--out', type=Path, default=Path('benchmarks/results/operations/macos-arm64.json'))
    parser.add_argument('--repeats', type=int, default=5)
    parser.add_argument('--workloads', nargs='+', choices=['source', 'tiny', 'large'], default=['source', 'tiny', 'large'])
    parser.add_argument('--seed', type=int, default=1)
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error('--repeats must be positive')
    binary = str(args.rgit.resolve())
    git = shutil.which('git')
    environment = {'platform': platform.platform(), 'machine': platform.machine(), 'python': platform.python_version(),
                   'git': capture([git, '--version']), 'rgit': capture([binary, '--version']),
                   'rgit_sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
                   'source_commit': capture([git, 'rev-parse', 'HEAD']),
                   'started_at_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                   'load_average_start': os.getloadavg() if hasattr(os, 'getloadavg') else None}
    if platform.system() == 'Darwin':
        environment.update(cpu=capture(['sysctl', '-n', 'machdep.cpu.brand_string']),
                           memory_bytes=int(capture(['sysctl', '-n', 'hw.memsize'])), macos=capture(['sw_vers', '-productVersion']))
    report = {'schema_version': 1, 'seed': args.seed, 'repeats': args.repeats, 'environment': environment,
              'operations': OPERATIONS,
              'method': ['CLI wall-clock milliseconds, including process startup; stdout discarded; no desktop UI timing.',
                         'Git commit includes git add -A plus git commit. Identity/configuration setup is not timed.',
                         'Fresh repositories each round; identical seeded bytes and 10 pre-measurement history commits.',
                         'Five files (four in large) and notes.txt modified per step. Branches diverge before merge.',
                         'One untimed status scan; no OS cache flushing. Runs alternate tool order each round.',
                         'Real loopback TCP for both: Git daemon git:// is unencrypted; RustGit TCP is authenticated/encrypted.',
                         'Host startup, invitation/enrollment and second pull-client clone are outside timings. No network shaping.',
                         'Git automatic maintenance, signing, hooks and fsmonitor disabled; no automatic GC before explicit final GC.',
                         'Default GC and integrity checks have different implementations/scopes; compare observed command cost, not equivalent guarantees.',
                         'Warm local filesystem and loopback results do not measure Windows, WAN/LAN throughput, or cold-disk performance.',
                         'All samples retained. Medians, min/max and Git/RustGit ratios derived without removing outliers.'],
              'workloads': {}}
    def save():
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, indent=2) + '\n')
    with tempfile.TemporaryDirectory(prefix='rustgit-operation-bench-') as temp:
        temp = Path(temp)
        for name in args.workloads:
            template = temp / (name + '-template')
            template.mkdir()
            dataset(template, name, args.seed)
            results = report['workloads'][name] = {'dataset': tree_digest(template), 'rounds': [], 'summary': {}}
            for repeat in range(args.repeats):
                row = {'round': repeat + 1, 'order': ['git', 'rgit'] if repeat % 2 == 0 else ['rgit', 'git'], 'tools': {}}
                print(f'{name}: round {repeat + 1}/{args.repeats}', flush=True)
                for tool in row['order']:
                    trial = Trial(tool, temp / f'{name}-{repeat}-{tool}', binary, name)
                    try:
                        row['tools'][tool] = trial.run(template)
                    finally:
                        trial.close()
                    shutil.rmtree(trial.root)
                assert row['tools']['git']['content_checks'] == row['tools']['rgit']['content_checks'], 'Tools produced different contents'
                results['rounds'].append(row)
                for op in OPERATIONS:
                    summary = {}
                    for tool in ['git', 'rgit']:
                        samples = [r['tools'][tool]['milliseconds'][op] for r in results['rounds']]
                        summary[tool] = {'median_ms': statistics.median(samples), 'min_ms': min(samples), 'max_ms': max(samples), 'samples_ms': samples}
                    summary['git_over_rgit'] = summary['git']['median_ms'] / summary['rgit']['median_ms']
                    results['summary'][op] = summary
                save()
    report['environment']['finished_at_utc'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    report['environment']['load_average_end'] = os.getloadavg() if hasattr(os, 'getloadavg') else None
    save()
    print(f'Results saved to {args.out}', flush=True)


if __name__ == '__main__':
    main()
