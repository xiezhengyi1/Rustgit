#!/usr/bin/env python3
"""Regenerate operation benchmark documentation and both README tables from raw samples."""
import argparse
import json
from pathlib import Path
import statistics

LABELS = {
    'init': ('初始化', 'Initialize'),
    'commit_initial': ('首次提交', 'Initial commit'),
    'status_clean': ('检查状态：无修改', 'Status: clean'),
    'status_dirty': ('检查状态：有修改', 'Status: modified'),
    'diff': ('查看工作区差异', 'Working-tree diff'),
    'log': ('历史（11 条）', 'History (11 commits)'),
    'show': ('查看单个提交', 'Show one commit'),
    'commit_incremental': ('增量提交', 'Incremental commit'),
    'branch': ('创建分支', 'Create branch'),
    'switch_old': ('切到初始版本', 'Switch to initial version'),
    'switch_back': ('切回最新分支', 'Switch back to main'),
    'restore_file': ('恢复单个文件', 'Restore one file'),
    'shelve': ('暂存未提交工作', 'Shelve / stash'),
    'unshelve': ('恢复暂存工作', 'Unshelve / stash pop'),
    'revert': ('撤销提交', 'Revert commit'),
    'merge': ('合并分支（无冲突）', 'Merge (no conflicts)'),
    'push_initial': ('首次发送', 'Initial push'),
    'clone': ('克隆并落盘文件', 'Clone with checkout'),
    'push_incremental': ('增量发送', 'Incremental push'),
    'fetch_incremental': ('获取新提交', 'Incremental fetch'),
    'pull_incremental': ('拉取并合并新提交', 'Incremental pull + merge'),
    'push_noop': ('发送：已经最新', 'Push: up to date'),
    'fetch_noop': ('获取：已经最新', 'Fetch: up to date'),
    'pull_noop': ('拉取：已经最新', 'Pull: up to date'),
    'verify': ('完整性检查', 'Integrity check'),
    'gc': ('常规整理（GC）', 'Default GC'),
}
START, END = '<!-- operation-benchmark:start -->', '<!-- operation-benchmark:end -->'


def number(value):
    return f'{value:.2f}' if value < 10 else f'{value:.1f}'


def table(workload, language, ranges=False):
    header = ('| 操作 | Git（ms） | RustGit（ms） | Git / RustGit |' if language == 0 else
              '| Operation | Git (ms) | RustGit (ms) | Git / RustGit |')
    lines = [header, '| --- | ---: | ---: | ---: |']
    for key, labels in LABELS.items():
        item = workload['summary'][key]
        cells = []
        for tool in ['git', 'rgit']:
            result = item[tool]
            text = number(result['median_ms'])
            if ranges:
                text += f" ({number(result['min_ms'])}–{number(result['max_ms'])})"
            cells.append(text)
        lines.append(f"| {labels[language]} | {cells[0]} | {cells[1]} | {item['git_over_rgit']:.2f}× |")
    return '\n'.join(lines)


def update_readme(path, section, anchor):
    text = path.read_text()
    if START in text:
        before, rest = text.split(START, 1)
        _, after = rest.split(END, 1)
        text = before + START + '\n' + section + '\n' + END + after
    else:
        text = text.replace(anchor, START + '\n' + section + '\n' + END + '\n\n' + anchor, 1)
    path.write_text(text)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--result', type=Path, default=Path('benchmarks/results/operations/macos-arm64.json'))
    args = parser.parse_args()
    report = json.loads(args.result.read_text())
    for workload in report['workloads'].values():
        assert len(workload['rounds']) == report['repeats']
        for op in LABELS:
            for tool in ['git', 'rgit']:
                samples = [row['tools'][tool]['milliseconds'][op] for row in workload['rounds']]
                assert statistics.median(samples) == workload['summary'][op][tool]['median_ms']
    assert set(report['workloads']) == {'source', 'tiny', 'large'}
    env = report['environment']
    date = env['started_at_utc'][:10]
    source = report['workloads']['source']
    source_size = source['dataset']['bytes'] / 1048576
    tiny = report['workloads']['tiny']['summary']
    large = report['workloads']['large']['summary']
    large_git_clone = large['clone']['git']['median_ms'] / 1000
    large_rgit_clone = large['clone']['rgit']['median_ms'] / 1000
    tiny_git_push = tiny['push_initial']['git']['median_ms'] / 1000
    tiny_rgit_push = tiny['push_initial']['rgit']['median_ms'] / 1000
    zh = f'''## 与 Git 的操作速度对比

以下是 **{date}、{env['cpu']} / {env['memory_bytes'] // 1073741824} GB / macOS {env['macos']}** 上的 CLI 实测。使用相同的合成源码数据（起始 {source['dataset']['files']} 个文件、{source_size:.2f} MiB），每个工具从全新仓库重复 {report['repeats']} 轮，表中为耗时中位数；**越低越快**。Git 提交包含 `add + commit`。

{table(source, 0)}

`Git / RustGit` 大于 1 表示 RustGit 在此项较快，小于 1 表示 Git 较快。差异很小的项目可能处于运行波动范围内。RustGit 并非每项都更快。

**数据类型会改变结果：** 64 MiB 大文件组的完整克隆为 Git {large_git_clone:.2f} 秒 / RustGit {large_rgit_clone:.2f} 秒；5,001 个小文件组的首次发送为 Git {tiny_git_push:.2f} 秒 / RustGit {tiny_rgit_push:.2f} 秒。前者包含局部修改的大文件历史，不能推广到所有大文件；后者暴露了 RustGit 当前的小文件首次传输开销。

同步都走本机 TCP：Git 使用未加密的 `git://`，RustGit 使用身份验证和加密连接；不包含首次邀请与设备登记。完整性检查和 GC 的实现与保障范围不同，这里只比较命令耗时。结果包含进程启动开销，**不是 Windows、真实局域网或公网速度**。另测了 5,001 个小文件和 64 MiB 大文件，完整结果、每轮原始数据及复现命令见 [速度基准](docs/performance.md)。
'''
    en = f'''## Operation speed compared with Git

Measured CLI wall time on **{date}, {env['cpu']} / {env['memory_bytes'] // 1073741824} GB / macOS {env['macos']}**. Both tools use identical synthetic source data (initially {source['dataset']['files']} files, {source_size:.2f} MiB), starting from fresh repositories for each of {report['repeats']} rounds. Values are medians; **lower is faster**. Git commit timings include `add + commit`.

{table(source, 1)}

`Git / RustGit` above 1 favors RustGit; below 1 favors Git. Small differences may fall within run-to-run variation. RustGit is not faster at every operation.

**The workload changes the result:** full clone in the 64 MiB large-file case took Git {large_git_clone:.2f} s / RustGit {large_rgit_clone:.2f} s; initial push of 5,001 small files took Git {tiny_git_push:.2f} s / RustGit {tiny_rgit_push:.2f} s. The former includes localized large-file edits and is not a claim about every large-file project; the latter exposes RustGit's current small-file initial-transfer overhead.

Both synchronize over loopback TCP: Git uses unencrypted `git://`; RustGit authenticates and encrypts connections. Invitation/enrollment is outside the timing. Integrity checks and GC have different implementations and guarantees; these compare command cost only. Process startup is included. These are **not Windows, real LAN, or WAN measurements**. Additional workloads cover 5,001 small files and 64 MiB of large files. See the [performance guide](docs/performance.md) for all results, individual samples, and reproduction commands.
'''
    update_readme(Path('README.md'), zh.rstrip(), '## 可以用服务器托管吗？')
    update_readme(Path('README.en.md'), en.rstrip(), '## Can I host it on a server?')
    doc = f'''# Git / RustGit 操作速度基准 / Operation speed benchmarks

[中文 README](../README.md) · [English README](../README.en.md) · [存储占用基准 / Storage benchmarks](benchmarks.md)

## 测试环境 / Environment

- Date: {date}. CPU: {env['cpu']}; RAM: {env['memory_bytes'] // 1073741824} GiB; macOS {env['macos']}.
- Git: `{env['git']}`. RustGit: `{env['rgit']}`, release CLI, SHA256 `{env['rgit_sha256']}`.
- Python: {env['python']}. Load average at start: {', '.join(f'{x:.2f}' for x in env['load_average_start'])}; at end: {', '.join(f'{x:.2f}' for x in env['load_average_end'])}. This was a shared workstation, not an isolated performance lab.
- Each tool/workload runs {report['repeats']} times in fresh temporary repositories. Every sample is retained.
- Tables show **median milliseconds (minimum–maximum)**. Lower time is better. The ratio is Git median / RustGit median, not a statistical significance claim.

## 方法 / Method

同一轮中的两个工具使用完全相同的文件内容和修改步骤，并交替先后顺序（奇数轮 Git 先，偶数轮 RustGit 先）。初始版本后先建立 10 次历史修改，再测日常操作；历史命令上限 100 条，执行时实际有 11 条。每次修改前五个文件及 `notes.txt`，大文件场景修改四个文件及说明文件。分支合并使用真实分叉、无冲突的历史。

All timings include command process startup and discard stdout. Git commits include both `git add -A` and `git commit`; RustGit commits use its single command. Repository identity/configuration, dataset creation, host startup, invitations/enrollment and the second pull-client clone are outside timing. An untimed status scan primes both tools' stat caches before status measurement. The operating-system cache is not flushed. Results therefore describe warm local workflows, not cold disks or desktop rendering.

- 网络：两者均使用真实本机 TCP，Git 通过 `git daemon`，不使用本地硬链接克隆。RustGit 使用加密、设备授权和已固定的主机身份；Git 的 `git://` 没有加密与授权。这是两种实际实现的开销比较，不能把差异全部归因于存储算法；也不是 Git over SSH 对比。
- Synchronization: each tool pushes the same main-branch history, clones with file checkout, then sends the same new commit. Fetch and pull use **separate** clients that were both behind, so the timed pull includes receiving data rather than merely merging already-fetched objects. Up-to-date operations are reported separately.
- 验证：每轮比较两个工具的初始版本切换、合并、完整克隆和拉取结果的文件数量、字节数与内容哈希。另确认 fetch 没有改动工作文件，恢复、暂存和撤销得到了预期内容。检查均在计时之外。
- Git automatic GC/maintenance, signing, hooks and fsmonitor are disabled, with global/system config isolated. No explicit GC runs before synchronization measurements. Both tools retain their native storage/durability defaults.
- `restore`、shelve/stash、完整性检查与 GC 的实现和保障范围并不完全相同；RustGit 强制恢复会保留恢复快照。这里比较原生命令完成相应任务的耗时，不宣称两者的安全保障或清理效果等价。默认 GC 不等同于原有存储基准的 aggressive GC。
- Limitations: one seed, one Mac, warm cache, shared-machine load, small history. Five rounds provide observed ranges, not confidence intervals. Do not extrapolate to Windows, cold storage, WAN latency, large histories, Git over SSH, or a universal speedup.

## 数据与结果 / Datasets and results
'''
    for key, title, explanation in [
        ('source', '源码类 / Source-like', '300 generated text files, 200 constant-like lines each, plus notes.txt.'),
        ('tiny', '大量小文件 / Many small files', '5,000 files containing 512 bytes of seeded hexadecimal text, plus notes.txt.'),
        ('large', '大文件 / Large files', 'Four 16 MiB files of seeded pseudorandom bytes, plus notes.txt. Localized 64-byte edits per data file; this is not a broad large-file performance claim.'),
    ]:
        workload = report['workloads'][key]
        data = workload['dataset']
        doc += f"\n### {title}\n\n{explanation} Initially {data['files']:,} files, {data['bytes']:,} bytes ({data['bytes'] / 1048576:.2f} MiB). Two files are later added by the branch/merge scenario.\n\n"
        doc += table(workload, 0, ranges=True) + '\n'
    doc += '''
## 命令对应 / Command mapping

These are 26 common tasks, not every possible command or option. Git staging has no separate
RustGit equivalent; RustGit reindex has no single matching Git command. Conflicting merges, invitations,
revocation and desktop UI latency are outside these speed tables.

Network URLs, destination names, messages and fingerprint placeholders are replaced per round.
Each source/tiny edit touches five data files plus notes.txt; large edits touch four data files plus notes.txt.

| Operation | Git | RustGit |
| --- | --- | --- |
'''
    for key, commands in report['operations'].items():
        doc += f'| {LABELS[key][1]} | `{commands[0]}` | `{commands[1]}` |\n'
    doc += '''
## 复现 / Reproduce

From the repository root on macOS (Python 3.9+, Git and the release RustGit CLI):

```bash
cargo build --release -p rustgit-cli --features iroh
python3 scripts/benchmark-operations.py --repeats 5 --out benchmarks/results/operations/macos-arm64.json
python3 scripts/render-operation-benchmarks.py
```

The runner binds both test servers to loopback and uses isolated temporary identities and file keys.
It does not access your real projects or system credential store. Temporary files and servers are
cleaned up after the run. The renderer updates this guide and the marked sections in both READMEs
from the raw samples. A failed comparison stops the run instead of reporting that command as fast.

[原始逐轮数据 / Raw per-round results](../benchmarks/results/operations/macos-arm64.json) ·
[测试脚本 / Runner](../scripts/benchmark-operations.py) ·
[文档生成 / Renderer](../scripts/render-operation-benchmarks.py)
'''
    Path('docs/performance.md').write_text(doc)
    print('Updated README.md, README.en.md, docs/performance.md from verified raw medians.')


if __name__ == '__main__':
    main()
