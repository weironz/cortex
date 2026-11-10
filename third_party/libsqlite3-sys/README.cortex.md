# Cortex Windows WAL 恢复 backport

此目录保存固定的上游源码与 backport。目录存在不表示运行中产物已经启用；必须核对 Cargo 解析结果、构建 source proof 与安装包 SBOM。

原始包：`libsqlite3-sys 0.37.0`，crate checksum
`b1f111c8c41e7c61a49cd34e44c7619462967221a6443b0ec299e0ac30cfb9b1`。
Rust 绑定保留原 MIT LICENSE；SQLite amalgamation 保留原 public-domain blessing。

标准 SQLite 为 3.51.3，修复来自官方 check-in
`7c103a0b7d509dce58aa87518e13213451e5f70938ff08aa55accd645e3b2ff3`：

- <https://sqlite.org/src/info/7c103a0b7d509dce58aa87518e13213451e5f70938ff08aa55accd645e3b2ff3>
- <https://sqlite.org/forum/info/ba7afbd0e81501118563617430fe06d0aca052e55ece2b0d457f8e16fcf85bba>

仅在 `sqlite3/sqlite3.c` 中复制上游 `winZeroSharedMemory`，并替换
`winLockSharedMemory`。不能把 truncate 错误直接忽略：旧 SHM 索引必须失效，
SQLite 才会重放 WAL 并保留最近提交的 Prepared fence。写零失败仍须返回错误。
不重试 prepare/finish/driver，不改变 WAL/FULL/FK、身份和授权。

Rust API 与 crate 版本保持原值，以兼容 SQLx 0.9 的 `^0.37` 约束。
因此单看 crate 或 `sqlite_version()` 不能识别修复；必须核对 amalgamation 哈希与
来源记录。SQLCipher 的独立 amalgamation 未改动，当前 Cortex 不启用该功能。

更新此取件时应固定 path patch 与锁文件、保留上游许可和来源哈希、在 NOTICE 说明，
并让 source proof、构建指纹和 SBOM 覆盖本地路径依赖及 C 源码。
新增路径依赖不能仅标记成 Cortex workspace 的自有代码。
