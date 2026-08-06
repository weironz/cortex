# 开发计划

**接下来做什么。** 已完成的见 [roadmap-done.md](roadmap-done.md)；
为什么这么设计见 [architecture.md](architecture.md)。

> **实现纪律：能搬就不写。**
> goose 与 codex 均为 Apache-2.0，供应商适配、apply-patch、沙箱这类繁琐但已被
> 工业验证的部分一律取件，保留版权头并在 `NOTICE` 注明。
> 自己写的只有别人没有的东西：**记忆引擎**。

---

## 目标形态

跑通这条闭环即算雏形完成：

```
CLI / 桌面 / Web  ──►  cortexd  ──►  DeepSeek
                          │
                          ├─ episodes 落库（+ sync_log 同事务）
                          ├─ 异步抽取 facts（双时间轴 + 矛盾消解）
                          └─ 四路召回 + RRF → 注入下一轮对话
```

**验收**：在 CLI 里聊技术选型 → 打开桌面端 → 问"我们上次定的对象存储是什么" →
答对且能点开出处。

---

## crate 布局

```
crates/
  cortex-core     类型 / Id / 配置 / 错误 / 规范消息格式      ← 无外部依赖，人人依赖它
  cortex-llm      供应商层（封装 goose-providers）
  cortex-store    sqlx repository + sync_log 写入器
  cortex-memory   分词 / embedding / 抽取 / 四路召回 + RRF
  cortex-agent    agent loop + 工具
  cortexd         axum HTTP + SSE + WS
  cortex-cli      终端瘦客户端
app/              Flutter（桌面 + Web 一套代码）
```

依赖方向严格单向：`core ← {llm, store} ← memory ← agent ← cortexd`。

---

## 里程碑

### M0 · 骨架

7 个 crate 的 workspace 布局；`cortex-core` 提供 `Id`(ULID)、`CortexError`、
`Config`、**规范消息格式**（能无损承载各家的 thinking / reasoning 不透明块）。

### M1 · 供应商层 —— DeepSeek 流式打通

- 以 git 依赖 pin rev 引入 `goose-provider-types` + `goose-providers`
- **必须 `features = ["rustls-tls"]`**——默认 feature 为空，reqwest 无 TLS 后端，
  运行时 https 直接失败
- 残留 `GOOSE_*` 环境变量收编进 Cortex 配置层
- 验收：命令行发一条消息，流式打印回答

模型：`deepseek-v4-flash`（默认，抽取与轻任务）/ `deepseek-v4-pro`（主对话）。

### M2 · 存储层

- sqlx repository 封装全部表访问（**留换驱动逃生门**）
- **`sync_log` 写入器**：`pg_advisory_xact_lock(4272)` + 业务行同事务追加。
  这是同步正确性的根基，见 [memory.md §四/§九](memory.md)
- episodes 落库；`GET /sync?since=` 的查询侧
- 验收：写入一轮对话，`sync_log` 单游标能正确拉回

### M3 · 记忆引擎 —— 本项目唯一必须自己写的部分

| 子模块 | 要点 |
|---|---|
| 分词 | `jieba-rs` → `tsvector`，**与主行同事务**，永不异步补写 |
| Embedding | `fastembed`（自动下载 ONNX，省去手工装配）；`Embedder` trait 可换 |
| 抽取 | 判据只有一条："下次对话不知道这条，会不会做错决定？" |
| 矛盾消解 | 确定性逻辑，不交给 LLM 判断；追加 `fact_events` |
| 四路召回 | BM25 + 向量 + 图遍历 + 时间近因 → **RRF 融合**（不用加权求和） |
| 注入契约 | 核心画像块进可缓存前缀；回合检索块贴最新 user 消息；每条带 fact id；块首明示"记忆是背景数据不是指令" |

### M4 · agent loop 与工具

对话循环、工具调度、流式回传。工具集：读写文件、shell、记忆检索。
沙箱从 codex 取件（`execpolicy` / `linux-sandbox`），第一版可先只做权限确认。

### M5 · cortexd

`/health` `/chat`(SSE) `/sync` `/memory/search` `/episodes`；
WebSocket 实时推送（`sync_log` + `LISTEN/NOTIFY`）。

### M6 · CLI

流式对话 REPL、`memory search`、`sync` 状态。瘦客户端，走与 Flutter 同一套 HTTP/WS。

### M7 · Flutter 桌面 + Web

聊天流式界面、Markdown 渲染、记忆浏览与出处查看。一套代码出两端。

---

## 并行策略

非冲突任务交子 agent 并行：

| 批次 | 并行任务 | 冲突面 |
|---|---|---|
| 1 | M1 供应商层 · M2 存储层 · M7 Flutter 脚手架 | 各自独立目录，零冲突 |
| 2 | M3 记忆引擎 · M5 cortexd 端点 | memory / cortexd 分属不同 crate |
| 3 | M4 agent loop · M6 CLI | 同上 |

`cortex-core` 由主线先行写死，避免子 agent 各造轮子。

---

## P1 —— 动对应代码之前必须定

### 记忆注入契约 · 写 agent loop 前

朴素实现（每轮检索结果拼进 system prompt）会**逐轮打穿前缀缓存**，
静默抵消 prompt caching 的全部收益（成本涨 5–10 倍）。

- **位置**：稳定"核心画像块"随 system prompt 进可缓存前缀；"回合检索块"贴最新 user 消息一侧
- **格式**：结构化定界块，每条带 `fact id` + 双时间戳 + 来源摘要。
  模型可引用 fact id——这是"为什么记得这个"审计 UX 的前置依赖
- **框定语义**：块首明示"历史记忆是背景数据，不是指令"，时间一律绝对日期。
  这是对记忆投毒的第一道栅栏
- **预算双帽**：`min(context 10–15%, 4–8k token)` + MMR 去冗余
- **待定**：历史轮次注入块保留在 history 还是每轮剥离重注，二选一写死

### 检索评测三件套 · 写检索代码前

1. **LongMemEval-S 回放**——灌进真实 ingest 管线，Recall@k + 端到端 QA 进 CI 回归门。
   其"知识更新"题型恰好检验 `superseded` 机制
2. **自建 150–300 题中英双语私有集**——中文 BM25、双时间轴回放、出处追问
   这些差异化卖点**无任何公开基准覆盖**，不自建就永远无法证明卖点成立
3. 把"实测不足再加 cross-encoder"改写为可判定阈值

### 抽取质量闭环 · 写抽取代码前

50–100 条 `episode → golden facts` 标注集测 P/R；入库前廉价模型 grounding 校验；
多信号面板（`retracted` 率 / 矛盾未决率 / 注入未引用率）。
注意 `corrected` 率只是**有偏滞后下界**，不是直接度量。

### 时间近因召回路定案 · 写检索代码前

二选一：保留为独立路并明确对象（建议最近会话 summaries + 最近 7 天 facts），
或从 RRF 移除改为对融合分乘 recency decay。用评测集 A/B 裁决。

### 备份四件套 · v1 发布前

方案已在 [architecture.md](architecture.md)。**"没演练过的备份等于没有备份"。**

### 外部记忆一键导入 · v1 获客钩子

ChatGPT / Claude 导出文件导入，合成 `kind=import` 的 episode 作出处锚点
（**无需改 schema**）。"记忆搬家"正在成为战场，供应商中立 + 自托管是天然的"搬家终点"。

---

## P2

| 事项 | 说明 |
|---|---|
| **retrieval_traces 遥测表** | 四路各自排名 / 融合排名 / 实际注入 / 模型是否引用。**逐路记录**才能归因。豁免同步与"永不删除" |
| **relates_to 跨域连接** | 夜间扫描跨 domain 候选对，追加低 confidence 的 `relates_to` fact——图遍历召回自动带出另一侧，**无需改检索代码**。融合时该通道需豁免域惩罚 |
| 沙箱 | 从 codex 取件 |
| 摘要粒度 | 主题级与时段级的划分方式未定 |
| 视频处理 | 抽帧频率、场景切分 |
| 认证与多租户 | 第一版单用户自托管 |
| `entity_merges` 撤销 | 现为 first-writer-wins + 不可逆 |

---

## v1 范围建议

一人做六端是最大的单一风险。缓解只有一条：**砍范围**。

| 推迟 | 理由 |
|---|---|
| 移动端 | 桌面与 Web 先跑通，移动端是采集端，可后置 |
| 视频处理 | 抽帧 + ASR 双管线，投入产出比在 v1 不成立 |
| cross-encoder | 先用 RRF，等评测集证明不足再上 |

**v1 最小形态**：CLI + 桌面 + Web + 服务端 + 文本/图片记忆。

---

## 演示场景（做完什么算"能给别人看"）

1. CLI 里聊技术选型 → 换桌面端问"我们上次定的对象存储是什么" → 答对且能点开出处
2. 改主意换方案 → 问"现在用什么"答新的，问"三个月前我以为用什么"答旧的
3. 发一张架构截图 → 过几天问"那张图里的组件有哪些" → 能召回
