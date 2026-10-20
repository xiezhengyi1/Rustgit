# Git / RustGit 操作速度基准 / Operation speed benchmarks

[中文 README](../README.md) · [English README](../README.en.md) · [存储占用基准 / Storage benchmarks](benchmarks.md)

## 测试环境 / Environment

- Date: 2026-10-20. CPU: Apple M3; RAM: 8 GiB; macOS 27.0.1.
- Git: `git version 2.54.0 (Apple Git-157)`. RustGit: `rgit 0.1.0`, release CLI, SHA256 `3df6066302536b6412db119425ae527ca290139468eb956b64390f34fbb91238`.
- Python: 3.9.6. Load average at start: 11.84, 8.86, 9.24; at end: 7.55, 7.02, 7.75. This was a shared workstation, not an isolated performance lab.
- Each tool/workload runs 5 times in fresh temporary repositories. Every sample is retained.
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

### 源码类 / Source-like

300 generated text files, 200 constant-like lines each, plus notes.txt. Initially 301 files, 2,247,010 bytes (2.14 MiB). Two files are later added by the branch/merge scenario.

| 操作 | Git（ms） | RustGit（ms） | Git / RustGit |
| --- | ---: | ---: | ---: |
| 初始化 | 12.9 (12.2–15.2) | 13.4 (11.9–13.9) | 0.96× |
| 首次提交 | 167.6 (166.3–171.5) | 24.0 (23.8–25.1) | 6.98× |
| 检查状态：无修改 | 8.85 (8.41–44.3) | 7.46 (7.16–17.5) | 1.19× |
| 检查状态：有修改 | 9.50 (9.07–10.4) | 7.39 (7.06–10.3) | 1.28× |
| 查看工作区差异 | 8.85 (8.44–9.88) | 8.17 (7.80–8.96) | 1.08× |
| 历史（11 条） | 8.02 (7.71–8.71) | 5.25 (4.94–5.94) | 1.53× |
| 查看单个提交 | 8.74 (8.54–10.3) | 6.30 (6.03–27.1) | 1.39× |
| 增量提交 | 22.1 (21.8–24.9) | 12.3 (11.1–17.8) | 1.79× |
| 创建分支 | 7.99 (7.59–8.38) | 5.46 (4.97–7.27) | 1.46× |
| 切到初始版本 | 11.1 (10.7–12.2) | 8.08 (7.94–13.5) | 1.37× |
| 切回最新分支 | 11.7 (11.1–12.3) | 9.26 (8.57–35.0) | 1.27× |
| 恢复单个文件 | 8.16 (8.00–8.83) | 7.07 (6.57–7.41) | 1.15× |
| 暂存未提交工作 | 24.0 (23.1–26.9) | 16.0 (13.9–22.7) | 1.50× |
| 恢复暂存工作 | 11.0 (10.9–11.5) | 9.64 (8.94–24.2) | 1.14× |
| 撤销提交 | 13.2 (12.5–15.3) | 14.8 (13.9–17.9) | 0.89× |
| 合并分支（无冲突） | 16.9 (16.3–42.3) | 14.0 (12.3–17.2) | 1.21× |
| 首次发送 | 119.7 (118.9–121.9) | 1648.5 (1515.5–1785.2) | 0.07× |
| 克隆并落盘文件 | 69.7 (67.9–71.9) | 343.9 (325.1–357.6) | 0.20× |
| 增量发送 | 44.9 (44.1–49.9) | 55.5 (52.4–57.9) | 0.81× |
| 获取新提交 | 40.4 (39.8–42.4) | 24.0 (20.8–25.9) | 1.69× |
| 拉取并合并新提交 | 67.0 (65.8–100.5) | 25.2 (24.1–27.4) | 2.66× |
| 发送：已经最新 | 15.8 (15.5–16.2) | 10.7 (9.52–11.2) | 1.47× |
| 获取：已经最新 | 24.1 (23.5–26.5) | 7.78 (7.72–7.80) | 3.10× |
| 拉取：已经最新 | 32.8 (32.6–42.2) | 7.41 (7.36–7.44) | 4.42× |
| 完整性检查 | 49.1 (47.4–49.7) | 14.9 (14.1–17.1) | 3.29× |
| 常规整理（GC） | 186.4 (182.2–219.8) | 20.2 (16.9–25.7) | 9.23× |

### 大量小文件 / Many small files

5,000 files containing 512 bytes of seeded hexadecimal text, plus notes.txt. Initially 5,001 files, 2,560,010 bytes (2.44 MiB). Two files are later added by the branch/merge scenario.

| 操作 | Git（ms） | RustGit（ms） | Git / RustGit |
| --- | ---: | ---: | ---: |
| 初始化 | 17.5 (13.2–22.9) | 19.9 (16.1–32.9) | 0.88× |
| 首次提交 | 2057.9 (2022.8–2107.0) | 161.6 (155.3–162.3) | 12.73× |
| 检查状态：无修改 | 15.3 (14.4–17.0) | 32.6 (32.0–35.0) | 0.47× |
| 检查状态：有修改 | 17.0 (15.6–40.6) | 33.0 (32.2–35.4) | 0.51× |
| 查看工作区差异 | 12.8 (12.1–15.1) | 35.0 (33.9–36.3) | 0.37× |
| 历史（11 条） | 9.24 (8.21–9.71) | 5.57 (5.38–6.23) | 1.66× |
| 查看单个提交 | 9.60 (8.91–9.71) | 9.33 (9.27–13.9) | 1.03× |
| 增量提交 | 34.6 (33.9–40.2) | 41.5 (40.6–45.7) | 0.83× |
| 创建分支 | 9.05 (9.02–9.18) | 5.63 (5.60–6.09) | 1.61× |
| 切到初始版本 | 16.4 (15.8–26.6) | 25.7 (25.1–27.3) | 0.64× |
| 切回最新分支 | 17.5 (17.1–18.4) | 27.3 (26.6–27.6) | 0.64× |
| 恢复单个文件 | 9.30 (8.98–9.90) | 22.9 (22.5–24.8) | 0.41× |
| 暂存未提交工作 | 44.1 (41.6–46.1) | 111.6 (110.7–143.5) | 0.40× |
| 恢复暂存工作 | 19.4 (18.6–19.9) | 56.0 (54.3–58.8) | 0.35× |
| 撤销提交 | 20.6 (19.9–21.1) | 62.8 (60.7–72.0) | 0.33× |
| 合并分支（无冲突） | 29.9 (28.7–32.5) | 64.1 (61.8–73.7) | 0.47× |
| 首次发送 | 547.2 (540.1–588.7) | 20323.5 (19901.6–20507.4) | 0.03× |
| 克隆并落盘文件 | 390.1 (387.0–395.1) | 1103.5 (1083.8–1172.9) | 0.35× |
| 增量发送 | 48.3 (47.0–54.1) | 54.9 (49.9–59.0) | 0.88× |
| 获取新提交 | 43.1 (41.2–44.7) | 17.6 (15.0–21.2) | 2.44× |
| 拉取并合并新提交 | 185.3 (71.2–204.8) | 43.6 (40.6–50.3) | 4.25× |
| 发送：已经最新 | 18.3 (16.9–20.7) | 11.3 (8.54–15.0) | 1.62× |
| 获取：已经最新 | 26.2 (24.7–29.1) | 7.65 (7.37–7.77) | 3.42× |
| 拉取：已经最新 | 34.6 (33.6–38.0) | 7.34 (7.26–7.40) | 4.71× |
| 完整性检查 | 210.0 (205.0–234.0) | 51.3 (50.5–56.2) | 4.09× |
| 常规整理（GC） | 930.2 (920.4–952.3) | 52.5 (49.8–81.6) | 17.71× |

### 大文件 / Large files

Four 16 MiB files of seeded pseudorandom bytes, plus notes.txt. Localized 64-byte edits per data file; this is not a broad large-file performance claim. Initially 5 files, 67,108,874 bytes (64.00 MiB). Two files are later added by the branch/merge scenario.

| 操作 | Git（ms） | RustGit（ms） | Git / RustGit |
| --- | ---: | ---: | ---: |
| 初始化 | 16.6 (14.6–46.1) | 27.9 (18.1–36.2) | 0.59× |
| 首次提交 | 1248.7 (1241.4–1265.8) | 120.9 (104.5–145.5) | 10.33× |
| 检查状态：无修改 | 8.54 (8.08–8.61) | 74.2 (74.0–75.8) | 0.12× |
| 检查状态：有修改 | 124.3 (122.6–138.8) | 73.8 (73.7–75.8) | 1.68× |
| 查看工作区差异 | 140.0 (137.3–144.4) | 124.2 (123.4–129.6) | 1.13× |
| 历史（11 条） | 10.4 (9.49–10.9) | 5.79 (5.56–6.00) | 1.80× |
| 查看单个提交 | 28.4 (27.6–29.0) | 87.6 (87.0–90.3) | 0.32× |
| 增量提交 | 1249.3 (1219.2–1279.5) | 85.6 (84.8–88.2) | 14.60× |
| 创建分支 | 8.36 (8.31–8.79) | 5.79 (5.57–6.02) | 1.44× |
| 切到初始版本 | 192.0 (184.1–282.9) | 122.6 (119.5–122.8) | 1.57× |
| 切回最新分支 | 160.5 (80.6–160.9) | 126.7 (123.5–149.0) | 1.27× |
| 恢复单个文件 | 9.30 (9.07–10.2) | 6.49 (6.29–6.72) | 1.43× |
| 暂存未提交工作 | 603.6 (516.8–606.6) | 225.9 (214.0–236.0) | 2.67× |
| 恢复暂存工作 | 12.1 (11.4–128.1) | 74.7 (74.0–76.0) | 0.16× |
| 撤销提交 | 60.8 (58.9–173.9) | 196.4 (194.0–320.3) | 0.31× |
| 合并分支（无冲突） | 18.2 (15.8–139.1) | 79.8 (77.5–100.2) | 0.23× |
| 首次发送 | 20651.2 (20523.8–21226.4) | 1916.7 (1860.6–2309.1) | 10.77× |
| 克隆并落盘文件 | 6439.7 (5924.0–6529.0) | 690.0 (673.0–761.0) | 9.33× |
| 增量发送 | 1646.4 (1618.5–1696.2) | 67.9 (64.8–79.0) | 24.25× |
| 获取新提交 | 1663.4 (1598.5–1687.1) | 36.5 (31.7–41.8) | 45.55× |
| 拉取并合并新提交 | 1699.9 (1672.5–1710.6) | 153.8 (152.0–168.8) | 11.05× |
| 发送：已经最新 | 17.1 (16.4–17.7) | 11.8 (8.85–13.9) | 1.45× |
| 获取：已经最新 | 25.3 (24.8–29.5) | 7.76 (7.70–7.87) | 3.26× |
| 拉取：已经最新 | 34.2 (33.4–36.2) | 7.35 (7.34–7.41) | 4.66× |
| 完整性检查 | 2759.5 (2725.0–2894.2) | 88.3 (87.6–90.2) | 31.26× |
| 常规整理（GC） | 4651.7 (4553.0–4791.1) | 46.0 (40.8–49.0) | 101.10× |

## 命令对应 / Command mapping

These are 26 common tasks, not every possible command or option. Git staging has no separate
RustGit equivalent; RustGit reindex has no single matching Git command. Conflicting merges, invitations,
revocation and desktop UI latency are outside these speed tables.

Network URLs, destination names, messages and fingerprint placeholders are replaced per round.
Each source/tiny edit touches five data files plus notes.txt; large edits touch four data files plus notes.txt.

| Operation | Git | RustGit |
| --- | --- | --- |
| Initialize | `init -q -b main` | `init` |
| Initial commit | `add -A; commit -q -m initial` | `commit -m initial` |
| Status: clean | `status --porcelain=v1` | `status` |
| Status: modified | `status --porcelain=v1` | `status` |
| Working-tree diff | `diff --no-ext-diff` | `diff` |
| History (11 commits) | `log -n 100 --format=medium` | `log -n 100` |
| Show one commit | `show --no-ext-diff HEAD` | `show HEAD` |
| Incremental commit | `add -A; commit -q -m incremental` | `commit -m incremental` |
| Create branch | `branch feature` | `branch feature` |
| Switch to initial version | `switch -q first` | `switch first` |
| Switch back to main | `switch -q main` | `switch main` |
| Restore one file | `restore --source HEAD~1 -- notes.txt` | `restore --from HEAD~1 notes.txt` |
| Shelve / stash | `stash push -q -m wip` | `shelve -m wip` |
| Unshelve / stash pop | `stash pop -q` | `unshelve 1` |
| Revert commit | `revert --no-edit HEAD` | `revert HEAD` |
| Merge (no conflicts) | `merge -q --no-edit --no-ff feature` | `merge feature` |
| Initial push | `push -q origin main` | `push` |
| Clone with checkout | `clone -q git://127.0.0.1:PORT/host.git COPY` | `clone tcp://127.0.0.1:PORT COPY --host-key FINGERPRINT` |
| Incremental push | `push -q origin main` | `push` |
| Incremental fetch | `fetch -q origin` | `fetch` |
| Incremental pull + merge | `pull -q --no-rebase origin main` | `pull` |
| Push: up to date | `push -q origin main` | `push` |
| Fetch: up to date | `fetch -q origin` | `fetch` |
| Pull: up to date | `pull -q --no-rebase origin main` | `pull` |
| Integrity check | `fsck --full` | `verify` |
| Default GC | `gc --quiet` | `gc` |

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
