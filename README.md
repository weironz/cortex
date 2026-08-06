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

## 架构方向

```
        cortex-core (Rust)  ← 唯一的业务逻辑实现
              │
   ┌──────────┼──────────────────────┐
   │          │                      │
cortex-cli   flutter_rust_bridge   cortexd (Rust 服务端)
 (Rust)           │                      │
              Flutter app            Web (React)
        桌面 ×3 + 移动 ×2              走 HTTP
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

## 技术选型

| 层 | 选择 |
|---|---|
| 核心逻辑 | Rust |
| 服务端 | Rust + axum + sqlx |
| CLI | Rust |
| 桌面 / 移动 | Flutter + flutter_rust_bridge |
| Web | React |
| 数据库 | Postgres + pgvector |
| 对象存储 | RustFS（S3 兼容） |

## 文档

- [参考项目](docs/references.md) —— 同类 Agent 调研、许可证边界与借鉴策略
