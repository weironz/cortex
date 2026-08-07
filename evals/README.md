# cortex-evals —— 检索质量评测

在这个 crate 之前，RRF 的 k 值、各路召回宽度、域加权系数、预算截断
**全部只能凭感觉调**。它做的事只有一件：让每一次「优化」都有数字可以对照。

题集是**自建**的中英双语私有集。公开基准（LoCoMo / LongMemEval）全是英文，
且没有任何一家覆盖 Cortex 的差异化能力：jieba + tsvector 的中文 BM25、
双时间轴回放、跨 domain 检索、以及「应当召回不到」。不自建就无法证明卖点成立。

---

## 怎么跑

```bash
# 只校验题集，不连数据库 —— CI 的第一道门，秒级
cargo run -p cortex-evals -- validate

# 跑一遍，出基线数字（需要 Postgres：先 just up）
cargo run -p cortex-evals -- run

# 落盘两份报告
cargo run -p cortex-evals -- run --json baseline.json --md baseline.md

# 只跑某几道题，调 bad case
cargo run -p cortex-evals -- run --only q05 --keep-schema

# A/B：关掉域加权看差多少
cargo run -p cortex-evals -- run --ignore-domain

# CI 回归门：跌破门槛就以非零码退出
cargo run -p cortex-evals -- run --min-recall5 0.75
```

`DATABASE_URL` 从仓库根的 `.env` 读。`--mode llm` 会真调抽取模型
（需要 `DEEPSEEK_API_KEY`），把抽取质量也一起测进来。

**向量后端跟生产同一条选择逻辑**（`CORTEX_EMBED_BACKEND`，默认 `fast`）。
早期版本在这里写死 `HashEmbedder`，代价是第一份基线全部建立在
「向量那一路只是第二条词法通道」的前提上 —— 换成真实语义模型后
逐路结论整体推翻，白调了一轮参数。无网环境显式设 `=hash`。

### A/B 与参数扫描

检索侧的每个旋钮都有对应的开关，用来回答「这个默认值凭什么」：

| 旋钮 | flag |
|---|---|
| 弃权阈值 | `--abstain-below 0.02` |
| 语义地板 | `--semantic-floor 0.45` / `--no-semantic-floor` |
| 时间近因形态 | `--recency channel\|decay\|off`（+ `--recency-strength/-half-life`） |
| episodes 一路 | `--episodes` / `--no-episodes` |
| 单路强命中补偿 | `--peak-bonus 0.04` |
| 时间回放排序 | `--replay-chronological`（退回改造前的全貌版） |

不给这些 flag 时**跑的就是生产默认值**，所以 `run` 出来的永远是基线。

批量扫描用 `evals/sweep.py`（一次跑一组、把关键数字拉平成一张表）：

```bash
python evals/sweep.py recency          # 近因形态 A/B
python evals/sweep.py episodes         # episodes 一路 A/B
python evals/sweep.py floor:0.4,0.45,0.5,0.55   # 临时扫一段
```

它刻意不在 Rust 里：`cortex-evals` 的产物是**一次**运行的报告，把
「跑 N 次再对比」塞进去会让 CI 那条路径也背上一堆用不到的旋钮。

### 不会污染开发库

每次运行建一个**临时 schema**（`cortex_eval_<时间戳>_<pid>`），在里面跑一遍
migration，结束 `DROP SCHEMA ... CASCADE`。做法与
`crates/cortex-store/tests/common/mod.rs` 同款。崩溃残留的 schema 由下次运行
按岁数（30 分钟）清扫 —— 不做无差别清扫，那会删掉兄弟进程正在用的。

`--keep-schema` 保留现场供手工排查（报告结尾会打出 schema 名）。

---

## 两种模式

| `--mode` | 候选事实从哪来 | 用途 |
|---|---|---|
| `seed`（默认） | 题集里手写的候选，走 `Extractor::write_candidates` | 语料确定，跑一百遍结果一致，**能进 CI** |
| `llm` | 真调廉价模型，走 `Extractor::ingest` | 连抽取质量一起测，但随模型漂移，不适合做回归门 |

两种模式**共用**实体消解、矛盾消解、jieba 分词、向量化与 `write_txn` 落库 ——
差别只在候选事实从哪来。评的是整条链路，不是四条 SQL。

---

## 题集格式

题集是一个 JSON 文件（`suites/cortex-zh-en-v1.json`）。
不用 YAML 是因为工作区没有任何 YAML crate，为一个文件引入依赖不划算
（且 `serde_yaml` 已停止维护）。

```jsonc
{
  "name": "...", "version": "...",
  "scenarios": [{
    "id": "sc-storage",
    "domain": "coding",              // 落到 episode 与 fact 的 domain 列
    "turns": [{
      "text": "对话原文……",           // 落成 episode；llm 模式下是抽取器的输入
      "occurred_at": "2026-03-02",
      "checkpoint": "ckpt_minio",    // 可选：记下本轮提交后的系统时间
      "facts": [{
        "subject": "对象存储", "subject_kind": "concept",
        "predicate": "stores_in",
        "object_entity": "MinIO",    // 与 "object"（字面量）二选一
        "object_entity_kind": "tool",
        "statement": "Cortex 的对象存储第一版选用 MinIO。",
        "valid_at": "2026-03-02",    // 【事件时间】矛盾消解按它判先后
        "confidence": 0.9
      }]
    }]
  }],
  "questions": [{
    "id": "q001",
    "type": "chinese_semantic",      // 见下表
    "lang": "zh",                    // zh | en | mixed
    "query": "我们的对象存储最后用的是什么？",
    "domain": "coding",              // 传给检索器做域加权
    "as_of": "ckpt_minio",           // 可选：走系统时间回放而非日常召回
    "gold":      [["对象存储", "RustFS"]],
    "forbidden": [["第一版选用 MinIO"]],
    "note": "为什么写这道题"
  }]
}
```

### gold / forbidden 为什么按子串写

事实的 id 是落库时现生成的 ULID，题集里写不出来；`llm` 模式下连 statement
的措辞都由模型决定。**「这几个关键词都得出现在同一条事实的 `statement` 里」**
是唯一在两种模式下都成立的标识方式。

- 一个 `gold` 元素 = 一组子串，**全部**出现才算命中（AND）
- 多个 `gold` 元素 = 多条应召回的事实，Recall 是它们的比例
- `forbidden` 同样的规则，但**出现即扣分**：已被取代的旧值、话题相邻的干扰项
- 匹配大小写不敏感；只匹配 `statement`（predicate / object 不参与 —— gold 越松，分数越虚高）

### 题型

| `type` | 含义 |
|---|---|
| `chinese_semantic` | 换个说法问同一件事（语义换说法，绝大多数是中文题） |
| `exact_term` | 型号、端口、标识符、人名、邮箱 —— BM25 的主场 |
| `temporal_replay` | 「三个月前我以为什么」，必须带 `as_of` |
| `relational` | 从一个实体跳到它关联的事实 —— 图遍历的主场 |
| `cross_domain` | 在 coding 语境问 office 的事（或反之） |
| `unanswerable` | **库里根本没有答案**，`gold` 必须为空、`forbidden` 必须非空 |

`unanswerable` 这一类不是补充，是刚需：**只测召回率会奖励「什么都召回」的退化策略**
——把召回宽度调到无穷大，Recall@k 恒为 1。

---

## 怎么加新题

1. 需要新语料就加 `scenarios`，不需要就直接在 `questions` 里追加。
   新事实的 `predicate` 若想让它**取代**旧值，必须用
   `cortex_memory::extract::DEFAULT_FUNCTIONAL` 里的单值谓词
   （`decided_on` / `assigned_to` / `deadline` / `stores_in` …），
   并且两条的 `valid_at` 分得出先后 —— 否则只会被打成 `flag`，两条并存。
2. `cargo run -p cortex-evals -- validate` 过静态校验。
3. `cargo test -p cortex-evals` 过题集完整性检查。它会挡住：
   - gold / forbidden 匹配不到任何铺垫事实（死题 / 恒真断言）
   - gold 与 forbidden 命中同一条事实（这道题永远判不对）
   - 某个题型题量掉到 8 以下（单题抖动会淹没信号）
4. `cargo run -p cortex-evals -- run --only <新题 id>` 看单题结果。

题目 id 保持字典序，报告与基线 JSON 才能直接 diff。

---

## 指标怎么读

| 指标 | 含义 | 怎么用 |
|---|---|---|
| `R@1 / R@5 / R@10` | 前 k 名里召回了多少条 gold（宏平均，每题等权） | R@5 对应实际注入窗口，是主指标 |
| `R@∞` | 不限名次的召回率 | 与 R@5 的**差额就是排序欠的债**：差额大该修融合，差额小该修召回 |
| `MRR` | 首个命中 gold 的名次倒数 | 对头部排名敏感 |
| `命中均名次` | gold 命中时的平均名次 | 配合 R@∞ 定位「召回到了但排太后」 |
| `误召@5` | 有 `forbidden` 的题里，前 5 名混入干扰项的比例 | **越低越好**。它是防止退化策略的那把锁 |
| `平均注入条数` | 每题最终注入多少条记忆 | 「应召不到」那一行不为 0，就说明检索器**无法弃权** |
| `gold 缺失` | gold 压根没落进库 | 这是**抽取或题目**的问题，不是检索的问题，已排除在 Recall 之外 |

### 逐路贡献

「覆盖」= 这一路在自己的原始召回列表里捞到了 gold 的题数。
「**独占**」= 只有这一路捞到的题数 —— **它才是某一路该不该保留的依据**。
覆盖高但独占为 0，说明这一路完全能被别的路替代。

「送进 top5 的干扰项」把噪声归因到具体通道，否则误召只能整体怪罪于「融合」。

**逐路诊断永远发全部五路**，与融合侧是否启用它们无关 ——「关掉这一路之后
它本来能捞到什么」正是 A/B 时最想看的那个数字，跟着开关一起关掉就只剩一列零。

时间回放题走的是**快照内**的三路（同一份 as-of 集合上的 bm25 / 向量 / 时间倒序），
与日常召回查的不是同一批数据，混在一张表里两套口径都读不了，
所以仍然从逐路分母里剔除；它的排序质量由主指标衡量。

### 分组不是可选项

总分掩盖问题。第一份基线的「整体 R@5 = 0.78」看着还行，但它是
「专名精确 0.92 + 中文语义 0.69 + 时间回放 0.42」平均出来的 ——
真正该修的是后两个。所有指标都按题型与语种各出一份。

---

## 进 CI 的建议

```yaml
# 第一道门：秒级，不需要任何服务
- run: cargo run -p cortex-evals -- validate
- run: cargo test -p cortex-evals

# 第二道门：需要 Postgres service
- run: cargo run -p cortex-evals --
         run --mode seed
             --json evals-report.json
             --min-recall5 0.75
- uses: actions/upload-artifact@v4
  with: { name: evals-report, path: evals-report.json }
```

`--min-recall5` 是整体门槛。更严的做法是把上一次的 `evals-report.json` 存成
基线，逐题型比 `by_kind[*].recall["5"]` —— 只比总分的话，某一类崩了而总分微跌，
门就放过去了。`Report::gate_metrics()` 已经把该比的数字摊平成
`kind.中文语义.recall@5` 这样的扁平 map，直接拿去 diff 即可。

**`--mode llm` 不要进回归门。** 它的结果随模型版本漂移，会变成随机红灯。
它的位置是「定期人工跑一次，看抽取质量」。

---

## 基线数字

自己跑一遍即可复现（`cargo run -p cortex-evals -- run`，不带任何 flag 就是
生产默认配置）。2026-08-07，`mode=seed`，
`embedder=fastembed:gpahal/bge-m3-onnx-int8`，73 条铺垫事实 / 111 道题：

| 题型 | R@1 | R@5 | R@10 | R@∞ | MRR | 命中均名次 | 误召@5 | 平均注入 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 全部 | 0.683 | 0.923 | 0.969 | 0.973 | 0.817 | 1.9 | 0.286 | 12.9 |
| 中文语义 | 0.758 | 0.939 | 1.000 | 1.000 | 0.839 | 1.8 | — | 14.0 |
| 专名精确 | 0.741 | 0.926 | 0.926 | 0.926 | 0.824 | 1.4 | — | 9.0 |
| 时间回放 | 0.667 | 0.833 | 0.917 | 0.944 | 0.744 | 2.8 | — | 27.2 |
| 关系推理 | 0.483 | 0.967 | 1.000 | 1.000 | 0.867 | 2.0 | — | 8.9 |
| 跨域检索 | 0.600 | 0.900 | 1.000 | 1.000 | 0.743 | 2.3 | — | 12.4 |
| 应召不到 | — | — | — | — | — | — | **0.500** | 10.4 |

逐路（分母 85）：`vector` 覆盖 85 / 独占 1 / R@5 0.976 · `bm25` 79/0/0.776 ·
`episode` 75/0/0.747（默认不入融合）· `graph` 43/0/0.482 ·
`recency` 32/0/0.094（默认不入融合）。

**读这张表时必须知道的四件事：**

1. **`专名精确` 的 R@∞ 是 0.926 而不是 1.000，这是已知的、故意付出的代价。**
   语义地板（余弦距离 > 0.50 即整体弃权）会误伤两道极短的专名查询
   （`赵敏是谁`、`jieba-rs`）—— 两三个字的 query 向量本来就发散。
   换来的是误召率 0.393 → 0.286。全曲线见 `retrieval::BGE_M3_SEMANTIC_FLOOR`。
2. **`应召不到` 那一行的误召 0.500 是这套检索当前最大的短板**，也是最难修的：
   干扰项是 `B302 会议室` 对 `B301`、`Redis 过期策略` 对 `pgvector` 这种
   语义上确实最近的东西。任何**基于分数**的闸门都分不开它们 ——
   弃权阈值从 0 扫到 0.030 这一列纹丝不动。见 `retrieval::abstain_below`。
3. **`R@∞` 几乎全线 1.000 是语料规模的产物**：69 条有效事实全部塞得进
   6000 token 的注入预算，预算截断从未生效。语料上千条之后这一列才有信息量。
   同理，**逐路独占数普遍为 0 不足以据此砍路** —— 每一路的召回宽度都覆盖了
   大半个库，独占本来就攒不出来。
4. **`Decay` 形态的近因在本题集上不可判**：全部事实是一次 ingest 里几秒内
   写完的，`created_at` 没有跨度，衰减系数对每条都一样，排序指标与
   `Off` 逐位相同。要判它，题集得先长出「跨越数周的语料」这一维。

### 已定案的旋钮

每个默认值背后都有一条扫描曲线，改之前先把曲线重跑一遍 ——
它们彼此有交互（弃权阈值就是被强命中补偿抬高的分布重新标定的）。

| 旋钮 | 定值 | 依据写在哪 |
|---|---|---|
| 时间近因 | **关**（既不做召回路也不做 decay） | `retrieval::RecencyMode` |
| episodes 一路 | **关**（实现与题目留着） | `retrieval::Retriever::episode_channel` |
| 单路强命中补偿 | 0.04 | `fusion::apply_peak_bonus` + `Retriever::new` |
| 弃权阈值 | 0.030 | `retrieval::Retriever::abstain_below` |
| 语义地板 | 0.50（仅真实语义后端） | `retrieval::BGE_M3_SEMANTIC_FLOOR` |
| 时间回放 | 快照内三路排序 | `retrieval::Retriever::retrieve_as_of_ranked` |
