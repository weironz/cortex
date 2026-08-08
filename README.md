# Cortex

> 记忆原生的通用 AI Agent —— 编码与办公通吃，对话永不丢失，跨设备实时同步。

## 这是什么

Cortex 是一个通用 AI Agent。与主流编码 Agent 的根本差别在于：**记忆是它的核心，而不是附加功能。**

- **永不丢失** —— 全链路 append-only，每一轮对话原样归档，无 UPDATE、无 DELETE
- **对话全模态归档** —— 你发给 agent 的一切（文本、图片、语音、视频）永久留存并可检索（注意：是对话内容的归档，不是 Rewind 式的屏幕/环境全量捕获）
- **云端同步** —— 记忆存于远端，任何设备连上即是完整的你
- **可审计** —— 每条记忆可追溯出处，可查看、可修改、可删除
- **事实演化** —— 记忆过期时标记取代而非抹除，可回答"三个月前为何如此决定"
- **不绑定厂商** —— 多 LLM 供应商自由切换

## 状态

**v0.1.0 —— 第一个可安装的版本。**

现在能做的：CLI / 桌面 / Web 三端聊天，记忆自动抽取与召回，
图片归档并可检索，会话绑定工作区后 agent 能读写文件，
bearer token 认证，Linux / macOS 上有 OS 级沙箱。
检索基线 R@5 0.877（116 题，真实向量），
恢复演练 RPO 46.9 s / RTO 43.6 s（[细节](docs/operations.md)）。

还不能做的：**离线**（瘦客户端，断网就用不了）、移动端、多用户、
Windows 上执行命令（没有对等沙箱，默认拒绝）、语音转录、桌面安装包。
逐条见 [CHANGELOG.md](CHANGELOG.md) 的「这一版**不能**干什么」。

## 安装

拿到的是二进制或镜像（不是这个仓库）→ **[docs/install.md](docs/install.md)**。
要部署到一台服务器并接 traefik → [docs/deploy.md](docs/deploy.md)。
要改代码 → `just setup && just bootstrap`，见 [operations.md](docs/operations.md)。

> **第一步一定是 `cortexd --generate-token`。**
> 没有配置凭据时 cortexd 会**拒绝启动** —— 这是刻意的：一个不认证的
> cortexd 会把整个记忆库交给任何能连上这个端口的人，而这件事没有任何症状。

## 架构

```
                 cortexd（远端，记忆权威）
      axum · Postgres+pgvector · RustFS · agent 循环 · 工具执行
                        ▲  HTTP / SSE / WS（同一套协议）
        ┌───────────────┼───────────────┬──────────────┐
        │               │               │              │
    cortex-cli     Flutter 桌面    Flutter 移动    Flutter Web
                                      （未做）

   客户端一律是瘦客户端：只负责渲染与输入，不含业务逻辑。
   agent 循环与工具执行都在 cortexd 里 —— 换设备接上就是完整的你。
```

> ⚠️ **上面画的是现状，而它有一个已定案要改的缺陷。**
>
> 工具跑在 cortexd 进程内，也就是说 agent 读写的是**服务器**上的目录 ——
> **你本地的代码它看不见**。对一个自称「编码 + 办公」的 agent，
> 编码那一半现在不工作。这不是体验粗糙，是能力缺失。
>
> 2026-08 已定案：**agent 循环搬到本地**，cortexd 退化成记忆服务。
> 循环该待在它要操作的东西旁边（你的文件在你的机器上），
> 而记忆要跨设备、要永久，只能有一个权威副本 —— 两个「跟着走」的方向不同。
>
> 曾经的理由「工具必须和记忆在同一侧」是错的，代价是延迟与信任模型两头都差：
> 一轮几十次文件操作变成几十个跨国往返，且服务端从此能让你的机器跑 shell。
> 完整裁决、否决的备选方案与四步计划见
> [架构决策文档](docs/architecture.md) 与 [roadmap 的 D/E 两节](docs/roadmap.md)。
>
> **今天的代价**：没有离线能力，且本地文件用不了。都是已知缺口，不是疏忽。

服务端存储：

- **Postgres + pgvector** —— episodes / entities / facts / 向量索引
- **RustFS**（S3 兼容）—— 图片 / 音频 / 视频 / 大文本，SHA-256 内容寻址

关键设计约束：

| 决策 | 理由 |
|---|---|
| 全链路 append-only | 多端并发写天然无冲突，同时满足"永不丢失" |
| ULID 主键 | 全局唯一、时间有序，多端无需协调即可生成 |
| daemon-first | 单写者、共享 embedding 模型常驻、后台任务、跨端连续性；记忆权威唯远端 cortexd |
| sync_log outbox | 同步的唯一事实序：单游标、不漏行、天然 FK 序，兼作实时推送事件源 |
| ~~客户端不含业务逻辑~~ | **这一条已被推翻**（2026-08）。现状确实是 agent 循环与工具都在 cortexd，但那导致 agent 看不见你本地的文件。定案是把循环搬到本地、cortexd 只管记忆。见上面架构一节 |
| 记忆 schema 领域无关 | entity / fact / relation 抽象，编码与办公通吃 |
| 双时间轴 | 区分"事情何时变"与"我何时知道"，支撑可审计与历史回放 |

## 技术选型

| 层 | 选择 |
|---|---|
| 核心逻辑 | Rust |
| 服务端 | Rust · axum · sqlx |
| CLI | Rust |
| **全部图形界面** | **Flutter**（桌面 ×3 + Web；移动 ×2 未做） |
| 客户端 ↔ 服务端 | HTTP / SSE / WS —— 不用 flutter_rust_bridge，客户端不链 Rust |
| 数据库 | Postgres + pgvector |
| 对象存储 | RustFS（S3 兼容） |
| Embedding | bge-m3 / 1024 维，本地 ONNX |
| 中文分词 | jieba-rs（Rust 侧处理，不依赖 PG 扩展） |

选型理由、被否决的备选方案与已接受的代价，见 [架构决策文档](docs/architecture.md)。

## 文档

| 文档 | 回答什么问题 |
|---|---|
| [roadmap.md](docs/roadmap.md) | **接下来做什么** |
| [roadmap-done.md](docs/roadmap-done.md) | 已经做了什么 |
| [architecture.md](docs/architecture.md) | 为什么这么设计（决策、否决的备选、代价、风险） |
| [memory.md](docs/memory.md) | 记忆系统怎么工作（存储模型、双时间轴、检索、同步） |
| [memory-content.md](docs/memory-content.md) | **记忆体存什么、不存什么**（五类输入 × 三种处理，含业界调研） |
| [operations.md](docs/operations.md) | 怎么部署、备份、恢复（含实测 RPO/RTO） |
| [references.md](docs/references.md) | 同类项目调研与许可证边界 |

schema 的权威版本是 [`migrations/`](migrations/)，文档中的 SQL 片段以它为准。

## 许可证

[Apache License 2.0](LICENSE)。

取件自 goose 与 codex 的部分见 [NOTICE](NOTICE) —— 两者同为 Apache-2.0，
每处出处也写在对应源文件的头部注释里。
