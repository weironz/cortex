# SQLite 首连诊断与运行时测试目录

> 2026-11-09。最终完整回归已通过，见末节；这不等于首连超时唯一根因已定案。

## 已确认的事实

主线连接池改为单连接后，五秒首连超时仍出现，说明不能单凭池与 Runtime 的
生命周期问题解释全部失败。typed 错误将失败限定在 connect_with；SQLx 0.9
此阶段包括 worker 建立、SQLite open 和初始化 PRAGMA，不能再细分为某一步。

当前开发机 D 盘是 SATA HDD，C 盘是 NVMe SSD。一次采样 D 盘为 0% idle、
队列长 5；空余内存约 19GB，磁盘空间充足。该采样不证明所有失败期间的负载，
也没有停止其他项目的进程或调整系统安全设置。

独立诊断复制 SQLx 0.9 源码到 target 内，仅在副本中记录匿名 phase 与耗时。
它没有修改全局 registry、主线依赖或首连预算。同类 C/D 测试使用同一 SHA 的
二进制，仅改变私有测试数据根：

| 项目 | D 盘数据 | C 盘数据 |
|---|---:|---:|
| unit | 5/5，1.95 秒 | 5/5，0.17 秒 |
| 集成 | 17/17，10.50 秒 | 17/17，0.96 秒 |
| 集成 PRAGMA 最大耗时 | 2061.822 ms | 26.597 ms |
| 集成 connect 完成最大耗时 | 2062.428 ms | 698.640 ms |

各最大值可能来自不同连接。四组均未复现 ConnectionTimeout，因此没有
“超时前最后完成的 phase”证据；不能指定唯一根因。查询日志的生成时机也
意味着缺少日志不等于 worker 未建立。

另用 Python SQLite 3.50.4、十二并发、真实 fsync/WAL/FULL 做独立 I/O 对照：
D 盘配置最大 2.409 秒、全程 4.330 秒，C 盘分别 22ms、44ms。它只支持存储
延迟差异，不是 SQLx 正确性或根因证明。

## 主线改变

旧测试将 env!(CARGO_MANIFEST_DIR)/test-data 或编译机仓库 tmp 硬编码进
测试程序。这不适用于便携执行，也使数据库与编译缓存争用源码盘随机 I/O。

两处核心测试 helper 改用运行机的 tempfile 私有目录；local 账本测试改用
系统临时目录下的专用唯一子目录，并保留失败证据。未指定固定盘符，也不改
生产机器权威的 CommonAppData/HKLM 位置。若另一台机器系统临时目录在 HDD，
测试仍会使用它；不据此宣称仅支持 SSD。

保留全部事务、fence、持久化、关闭重开和十二并发检查，未跳过测试、降低
并发或放宽五秒预算。新的主线核心 unit 5/5、集成 17/17 已通过，分别 0.03 秒
和 0.26 秒。后续完整 CI 的核心 5/17/2 项也通过，但 local 608 通过/1 失败，
暴露重复 provision 与旧连接关闭之间的 sidecar 竞态。

## WAL/SHM 生命周期协调

旧连接释放自己的 sidecar 保护后，最后一个 SQLite close 可能删除 WAL/SHM；
另一实例同时执行 prepare/bind sidecar，产生“文件不存在”。关闭现与打开、
绑定使用同一 writer 协调锁。对短暂争用作有限等待，超时不授予权限；取锁
失败时，sidecar 保护保留到连接退出，不清 pending/fence。

writer/ACL 协调等待设五秒上界；这是物理互斥等待，不是恢复权限，也没有放宽
SQLite 首连五秒 deadline。没有无界 File.lock。Actor 的等待及关闭仍在专用
阻塞线程，health 的单次八十毫秒预算保持不变。

新增真实文件锁超时不授予/不释放他人锁，以及持有 writer 时禁止先关闭
SQLite 的回归。整个 computer_ledger 模块以 32 并发运行，39 通过/2 专用入口
忽略。静态审查未见 ACL → writer 与 close 仅取 writer 的反向锁序。最终完整
CI 的该轮日志为 target/native-owned-final-coordinated-workflow-ci.log；后续修正
测试模块布局和 Dart 空值感知风格后，最终结果见下节。

## 最终结果

`just workflow-ci` 退出 0；日志 target/native-owned-verified-workflow-ci.log。
Rust 顶层 2018 通过/32 忽略，local 611 通过/23 忽略；Flutter 1092 通过/7 跳过，
analyzer 与严格 clippy 全部通过。没有跳过原门禁、降低并发或增加 SQLite
首连预算。最终真实 HTTP/Cua 输入回放也通过，见
[Native 验收](25-native-owned-runtime-validation-2026-11-09.md)。

## 证据与限制

- 副本与 phase 日志：target/sqlite-connect-diagnostic/RESULTS.md 及 logs。
- 原失败：target/native-owned-workflow-ci-single-connection.log，5 通过/12 超时失败。
- 新核心验证：target/native-owned-system-temp-core-unit.log、target/native-owned-system-temp-core-integration.log。
- Python 对照：target/sqlite-storage-probe.json，仅包含合成数据库与耗时。

高负载或慢盘下的首次初始化可用性仍需验收。超时不得放行输入、重建现有
历史或清除 fence；主线仍保持这一边界。当前不是生产 GA，未提交或发布。
