# Cua 上游版本只读复核（2026-11-10）

> 本次只读查询公开 GitHub 元数据和源码，未下载、安装或执行新版本；
> 正式 pin 与正在运行的 C3 均保持不变。新版本不代替已有或待完成的实测。

## 已核实的发布身份

GitHub release 列表本次返回正常版本 `cua-driver-rs-v0.45.4`，发布于
2026-11-09 15:19:15 UTC；tag 指向完整提交
`d96e1bc359b2cc638c7a406aad01a7276b1b416f`。
来源：[release](https://github.com/trycua/cua/releases/tag/cua-driver-rs-v0.45.4)、
[tag ref](https://api.github.com/repos/trycua/cua/git/ref/tags/cua-driver-rs-v0.45.4)。

GitHub 元数据的 `prerelease=true` 不能单独当作 nightly：上游发布说明明确其
monorepo 为保留跨产品 Latest 指针使用此标签，plain SemVer 属其稳定渠道。
这只说明上游渠道语义，不能证明 Cortex 兼容、Windows Authenticode 有效或缺陷已修复。

发布的 Windows x86-64 binary zip 摘要为
`949927b4cb0519c6fb1377878d67d79a8d6fae7bc56cbed17c7aa26448d82cb5`，
发布说明及 GitHub asset digest 一致。没有取得或核对 EXE 的 Authenticode，
不能把 GitHub commit 的 Verified 或 Sigstore 附件当作代码签名已经通过。

## 固定提交的所有权链复核

固定提交中的
[uia/mod.rs](https://github.com/trycua/cua/blob/d96e1bc359b2cc638c7a406aad01a7276b1b416f/libs/cua-driver/rust/crates/platform-windows/src/uia/mod.rs)
仍可看到 `UiaNode.element_ptr` 原始字段，以及 actionable/content 节点均 retain 后
转为原始指针的结构；非 actionable content 的 `element_index` 为 None。
[uia/snapshot.rs](https://github.com/trycua/cua/blob/d96e1bc359b2cc638c7a406aad01a7276b1b416f/libs/cua-driver/rust/crates/platform-windows/src/uia/snapshot.rs)
的 `from_nodes` 仍仅收集带 element_index 的节点。

| 文件 | Git blob 身份 |
|---|---|
| uia/mod.rs | `e96174062390b7132a3b8ca04ff0781847f63a00` |
| uia/snapshot.rs | `95787ca033f81d69a6c9efcab3ded74e40a4edcb` |

不能仅凭这两处结构推断泄漏；本次随后限定读取 8 个文件核对调用链，确认
`get_window_state` 没有为未接管的 nonactionable 节点补偿释放：

| 路径 | 已证实的所有权事实 |
|---|---|
| [get_window_state](https://github.com/trycua/cua/blob/d96e1bc359b2cc638c7a406aad01a7276b1b416f/libs/cua-driver/rust/crates/platform-windows/src/tools/get_window_state.rs#L385-L400) | 完整 tree 与只接管 indexed 节点的 payload 一起返回；发布/序列化及结束作用域没有释放被遗漏的 raw 指针 |
| [MSAA](https://github.com/trycua/cua/blob/d96e1bc359b2cc638c7a406aad01a7276b1b416f/libs/cua-driver/rust/crates/platform-windows/src/msaa.rs#L168-L233) | 有名字但不可操作的节点也 retain，再设置 element_index None，经过相同遗失路径 |
| [UIA fallback](https://github.com/trycua/cua/blob/d96e1bc359b2cc638c7a406aad01a7276b1b416f/libs/cua-driver/rust/crates/platform-windows/src/uia/mod.rs#L471-L503) | 替换 primary 或放弃 fallback 向量时，没有释放其中已 retain 的 raw 引用 |
| [Snapshot payload](https://github.com/trycua/cua/blob/d96e1bc359b2cc638c7a406aad01a7276b1b416f/libs/cua-driver/rust/crates/platform-windows/src/uia/snapshot.rs#L102-L140) | RetainedElement.Drop 确实释放已接管的 actionable 元素，但无法回收未进入 payload 的 nonactionable 引用 |
| [截图错误分支](https://github.com/trycua/cua/blob/d96e1bc359b2cc638c7a406aad01a7276b1b416f/libs/cua-driver/rust/crates/platform-windows/src/tools/get_window_state.rs#L432-L484) | 被丢弃的 payload 有 Drop，裸 nonactionable 指针仍无所有者 |

节点数/深度/协作式超时 admission 在 emit 前拒绝新节点，不额外 retain；但之前已发出
并返回的 nonactionable 节点仍走上述路径。存在 `release_walk_nodes` helper，
但它用于菜单或另一条 `walk_native_tree` error 分支，不能证明 `get_window_state`
调用的 `walk_tree_budgeted` 已修复。

结论仅限源码所有权：固定提交仍有 retained COM 引用遗失，不能删除本地修复或直接
以新版替换。未运行新版 driver，不报告其真实内存增速；外层 backstop timeout 后
blocking task 的实际存活时长、panic 增量也没有动态证据。

## 当前取舍与后续门禁

1. 保留当前官方 0.38.0 pin、签名和产品能力边界，不在长测中更新驱动。
2. C3 继续验证精确 a91f793a… 本地候选；不得将其结果移植到 0.45.4。
3. 若后续采用新版本，仍需移植完整 RAII 所有权修复或取得等效官方修复；不按
   版本号或简短 changelog 推断修复。移植后的产物单独走构建、签名和回归门。
4. 若列为升级候选，单独核对归档/EXE 摘要、Authenticode、许可证/SBOM、工具 schema、
   bounded manifest、AX-only、secure-field、窗口/ref 失配、取消/Job、实机动作、内存长测
   和产品 HTTP/审批链。未通过前不改默认配置。

当前本地未签名候选的可复现构建、短 probe 和 C3 状态仍以
[29](29-cua-repro-steady-validation-2026-11-10.md) 为准。
