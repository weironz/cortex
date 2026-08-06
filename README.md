# Cortex

> 记忆原生的通用 AI Agent —— 编码与办公通吃，对话永不丢失，跨设备实时同步。

## 这是什么

Cortex 是一个通用 AI Agent。与主流编码 Agent 的根本差别在于：**记忆是它的核心，而不是附加功能。**

- **永不丢失** —— 全链路 append-only，每一轮对话原样归档，无 UPDATE、无 DELETE
- **全模态** —— 文本、图片、音频、视频统一入库并可检索
- **云端同步** —— 记忆存于远端，任何设备连上即是完整的你
- **可审计** —— 每条记忆可追溯出处，可查看、可修改、可删除
- **事实演化** —— 记忆过期时标记取代而非抹除，可回答"三个月前为何如此决定"
- **不绑定厂商** —— 多 LLM 供应商自由切换

## 状态

**早期开发中，尚无可用版本。**

## 架构

```
                    cortex-core (Rust)
                    唯一的业务逻辑实现
                           │
        ┌──────────────────┼──────────────────┐
        │                  │                  │
   cortex-cli          cortexd          flutter_rust_bridge
     (Rust)         (Rust · axum)              │
                          │              Flutter 单一 UI 代码库
                      HTTP / WS                │
                          └────────┬───────────┘
                                   │
                  桌面 Win/macOS/Linux · 移动 iOS/Android · Web
                            —— 六个平台，一套 UI ——
```

服务端存储：

- **Postgres + pgvector** —— episodes / entities / facts / 向量索引
- **RustFS**（S3 兼容）—— 图片 / 音频 / 视频 / 大文本，SHA-256 内容寻址

关键设计约束：

| 决策 | 理由 |
|---|---|
| 全链路 append-only | 多端并发写天然无冲突，同时满足"永不丢失" |
| ULID 主键 | 全局唯一、时间有序，多端无需协调即可生成 |
| daemon-first | 单写者、共享 embedding 模型常驻、后台任务、跨端连续性 |
| 客户端本地 SQLite | 仅作缓存与离线写队列，**非**真相来源 |
| 记忆 schema 领域无关 | entity / fact / relation 抽象，编码与办公通吃 |
| 双时间轴 | 区分"事情何时变"与"我何时知道"，支撑可审计与历史回放 |

## 技术选型

| 层 | 选择 |
|---|---|
| 核心逻辑 | Rust |
| 服务端 | Rust · axum · sqlx |
| CLI | Rust |
| **全部图形界面** | **Flutter**（桌面 ×3 + 移动 ×2 + Web） |
| Rust ↔ Flutter | flutter_rust_bridge |
| 数据库 | Postgres + pgvector |
| 对象存储 | RustFS（S3 兼容） |
| Embedding | bge-m3 / 1024 维，本地 ONNX |
| 中文分词 | jieba-rs（Rust 侧处理，不依赖 PG 扩展） |

选型理由、被否决的备选方案与已接受的代价，见 [架构决策文档](docs/architecture.md)。

## 文档

- [架构与技术选型](docs/architecture.md) —— 决策记录、否决的备选方案、已接受的代价
- [记忆系统设计](docs/memory.md) —— 存储模型、双时间轴、检索策略与 schema
- [参考项目](docs/references.md) —— 同类 Agent 调研、许可证边界与借鉴策略
