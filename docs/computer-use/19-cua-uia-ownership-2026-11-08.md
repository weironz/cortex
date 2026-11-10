# Cua 0.38 UIA 所有权与长跑内存调查

> 2026-11-08。功能长测完成；内存稳态和修复后的驱动尚未验收。

## 本次真实结果

冻结指纹 `f4ee6186ffb9781af6294cf158e8a842f6ad17fb7c4b07a37a84d4e0b4c82570`
在 16:17—20:17 完成四小时：1385 个循环、139 次 Cua session、1 次只读重试，
退出码 0。敏感字段接管通过，fixture 清理前仍存活，前后指纹一致。

原始结果：[soak-0.1.45-lockguard-4h.json](../../tests/fixtures/computer-use-windows/results/soak-0.1.45-lockguard-4h.json)。
该结果不覆盖后来新增的审批关联、AX-only、独立凭据或产品 `/chat` 全链路。
旧测试的最终 `close` 没有断言错误，不能以退出码证明每次清理成功。

资源采样从 16:41 才开始，共 216 次，结束于测试退出：

| 进程 | 私有内存首值 → 末值 | 句柄首值 → 末值 / 最大值 |
|---|---|---|
| Cortex 测试 | 7.32 → 8.32 MiB | 435 → 435 / 441 |
| Cua driver | 27.27 → 127.04 MiB | 493 → 493 / 509 |
| fixture | 122.76 → 153.29 MiB | 959 → 874 / 960 |

这是中途开始的观察，没有强制的内存断言，不能据此宣布无泄漏。
数值证据：[soak-resource-analysis-2026-11-08.json](../../tests/fixtures/computer-use-windows/results/soak-resource-analysis-2026-11-08.json)。

## 固定源码中已找到的释放缺口

审计来源为 Cua tag `cua-driver-rs-v0.38.0`，commit
`85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19`，通过本地 `git show` 核对。

`platform-windows/src/uia/mod.rs::emit_cached_node` 对可操作或有内容的节点
clone COM element 后 `mem::forget`，把引用放入裸指针；非可操作节点也会走
这个路径，但没有 element_index。

`uia/snapshot.rs::UiaSnapshot::from_nodes` 只接收有 element_index 的节点。
未接收的节点不进入 `RetainedElement::drop`，`UiaNode` 也没有对应 Drop。
因此每次命中“有内容但不可操作”节点，都存在未配对的 COM 引用。
后续 `end_session` 清空已接收的 snapshot，不能释放这些遗失的引用。

这是静态代码可证的所有权缺口；尚未通过受控实验证明它解释了本次全部内存增量。
AX-only 仍会经过此路径，关闭截图不能代替修复引用所有权。

## 其他保留与验收顺序

- 会话墓碑、适配层 scopes/ended、capture generation 表没有数量或 TTL 上限；
  当前没有证据显示它们整包保留了已关闭会话的截图或 UIA 树。
- PNG registry 默认 32 张、64 MiB、60 秒 TTL；这些上限只覆盖自身持有的 PNG。
- snapshot 按窗口替换、每 PID 最多 8 份；正常 end_session 会移除该 session。
- 内部 UIA blocking 调用超时后的底层取消，以及 COM 初始化配对仍待单独评估。

先在隔离源码中修复引用所有权，使用 fake COM 计数验证成功、失败、截断和
重复转换的释放边界，再构建实际驱动。真实 A/B 应固定窗口、次数和频率，
分别比较 AX-only、截图、同名 session 复用和不断新建 session，记录预热后的
bytes/observation、清理结果和线程数。签名、SBOM、摘要和完整产品门仍须重新核对。

当前没有替换主线固定驱动，也没有向上游发送 issue 或代码。

## 2026-11-08：完整 Windows 候选构建

引用所有权修复已在独立目录通过平台真实编译，并完成完整 `cua-driver` release
构建，退出码 0；固定同一上游提交，manifest 与 Cargo.lock 前后哈希一致。
缺失的 registry 依赖只允许按 lock 的精确版本及 checksum 获取。

主线另行核对产物：38,469,632 bytes，SHA-256
`47179075f4ce1a36bda71e1939ceae0fbc124288923967a9965cd054d8089f5a`，
Authenticode 为 `NotSigned`。最小构建证明见
[cua-ownership-build-2026-11-08.json](../../tests/fixtures/computer-use-windows/results/cua-ownership-build-2026-11-08.json)。

该产物只用于受控调查，未进入随包驱动、未更改产品签名或 hash 门禁。
只读 fixture A/B 脚本已审查并纳入
[readonly-ab](../../tests/fixtures/computer-use-windows/readonly-ab/README.md)，
PowerShell 5.1 语法、46 项纯函数检查和管道排空检查通过。
脚本只允许唯一复制 fixture 的 `start_session/get_window_state/end_session`，
不授予输入或桌面访问；锁屏/安全桌面或身份不确定时停止。

11 月 9 日的小规模预热 3 次、观察 6 次两组均通过，结束会话及清理已验证。
结果见 [preflight](../../tests/fixtures/computer-use-windows/results/cua-readonly-ab-preflight-2026-11-09.json)。
每组预热 25 次、观察 200 次的比较也已完成，两组均为 PASS，结束会话与清理正常。

| 指标（预热后） | 官方分发包 | 修复候选 |
|---|---:|---:|
| 起始 private memory | 13.891 MiB | 12.371 MiB |
| 观察结束 private memory | 30.137 MiB | 13.797 MiB |
| 增量 | 16.246 MiB | 1.426 MiB |
| 采样线性斜率 | 79.007 KiB/observation | 3.140 KiB/observation |
| end_session 后 private memory | 30.141 MiB | 13.879 MiB |
| 平均观察耗时 | 1510.44 ms | 1539.54 ms |

原始数据见 [200 次结果](../../tests/fixtures/computer-use-windows/results/cua-readonly-ab-200-2026-11-09.json)，
数字摘要见 [分析](../../tests/fixtures/computer-use-windows/results/cua-readonly-ab-200-analysis-2026-11-09.json)。
这个有限只读条件下候选增长明显更低，但仍有增量，不能宣称全部泄漏或内存稳态门已关闭。
官方分发包与本地候选存在工具链差异，后续还需同工具链的无修改控制构建，
避免把编译差异误当成补丁的唯一效果。生产签名 gate 不受调查脚本影响。

## 同工具链无修改控制组

已用相同 Rust 1.98.0、CRT-static、release、Cargo.lock 与依赖输入构建控制驱动，
固定同一上游提交；六个被修改的既有文件恢复为该提交 Git blob，两个新增模块
不参与控制构建，其余 788 个文件与候选一致。主线复核 manifest/lock 未变及产物摘要：
`99daf64d73b961f324d7eea8063aeb3ddf6975ec7197b2a43cce04ac8c9e986e`，
38,471,680 bytes，未签名。证据见
[控制构建](../../tests/fixtures/computer-use-windows/results/cua-matched-control-build-2026-11-09.json)。

调查脚本新增独立 `MatchedControl` 参数集，要求控制构建证明及显式摘要，
报告不将其称作官方分发包；官方参数集的固定摘要与签名检查不变。
55 项源码自测及小规模控制组/修复组预热 3 次、观察 6 次均通过，
[preflight](../../tests/fixtures/computer-use-windows/results/cua-matched-ab-preflight-2026-11-09.json)。
同构建条件下每组预热 25 次、正式观察 200 次已完成，均为 PASS；会话结束、
fixture 与 daemon 清理、各 CLI 退出码检查通过。

| 指标（预热后） | 同工具链无修改控制组 | 修复候选 |
|---|---:|---:|
| 起始 private memory | 14.629 MiB | 11.949 MiB |
| 观察结束 private memory | 29.176 MiB | 12.484 MiB |
| 增量 | 14.547 MiB | 0.535 MiB |
| 采样线性斜率 | 73.509 KiB/observation | 3.531 KiB/observation |
| end_session 后 private memory | 29.176 MiB | 12.688 MiB |
| handles（开始 → 结束） | 479 → 481 | 479 → 481 |
| 平均观察耗时 | 2153.56 ms | 2184.53 ms |

原始结果见 [matched 200](../../tests/fixtures/computer-use-windows/results/cua-matched-ab-200-2026-11-09.json)，
摘要见 [matched 分析](../../tests/fixtures/computer-use-windows/results/cua-matched-ab-200-analysis-2026-11-09.json)。
在该有限条件下，增量约减少 96%，与非 actionable COM 引用释放缺口的静态分析一致。
仍有少量增长，不能证明全部路径无泄漏或最终四小时内存稳态；生产签名及产品回归要求不变。

## 上游更新核对

11 月 9 日通过 GitHub 官方接口核对到 Driver
[0.44.0 发布页](https://github.com/trycua/cua/releases/tag/cua-driver-rs-v0.44.0)，
不是把旧 0.38.0 当作当前最新版本。读取该 tag 所指
`ef1e84d6c880ab8aee7587c3d0057a288e764b69` 的源码后，
[emit_cached_node](https://github.com/trycua/cua/blob/ef1e84d6c880ab8aee7587c3d0057a288e764b69/libs/cua-driver/rust/crates/platform-windows/src/uia/mod.rs#L839)
仍对含内容节点 clone/forget COM 引用，
[UiaSnapshot::from_nodes](https://github.com/trycua/cua/blob/ef1e84d6c880ab8aee7587c3d0057a288e764b69/libs/cua-driver/rust/crates/platform-windows/src/uia/snapshot.rs#L126)
仍只接收带 element_index 的节点。

这两段路径仍采用原有所有权形式，未显示可直接替代本候选的节点 RAII 调整；
这里只是静态核对，不等于已经实测 0.44.0 的完整生命周期或宣布它存在相同增量。
尚未下载/执行或替换新版随包驱动；升级仍需独立评审 API、权限和签名兼容性。

11 月 9 日晚再次读取官方发布 API，确认
[0.45.2](https://github.com/trycua/cua/releases/tag/cua-driver-rs-v0.45.2)
已于 12:26 UTC 发布，指向 `ba5ab4449da9dbd97740895bd02bae866eb8d0cf`。
该提交的 [emit_cached_node](https://github.com/trycua/cua/blob/ba5ab4449da9dbd97740895bd02bae866eb8d0cf/libs/cua-driver/rust/crates/platform-windows/src/uia/mod.rs)
仍 clone/forget 含内容节点的引用；[from_nodes](https://github.com/trycua/cua/blob/ba5ab4449da9dbd97740895bd02bae866eb8d0cf/libs/cua-driver/rust/crates/platform-windows/src/uia/snapshot.rs)
仍只接收有 element_index 的节点。两段静态核对没有显示可直接替代本地 RAII
修复的变更，不等于已经测量新版内存增量。未下载执行、安装或更新主线 pin。
