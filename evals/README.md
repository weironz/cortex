# cortex-evals —— 检索质量评测

在这个 crate 之前，RRF 的 k 值、四路召回宽度、域加权系数、预算截断
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
    "as_of": "ckpt_minio",           // 可选：走系统时间回放而非四路召回
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

时间回放题走 `facts_as_of`，根本不发四路查询，已从逐路分母里剔除。

### 分组不是可选项

总分掩盖问题。「整体 R@5 = 0.78」看着还行，但它是
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

见仓库根的评测报告，或自己跑一遍。当前基线（2026-08-07，`mode=seed`，
`embedder=hash-stub-v1`，73 条铺垫事实 / 105 道题）：

| 题型 | R@1 | R@5 | R@10 | MRR | 误召@5 |
|---|---:|---:|---:|---:|---:|
| 全部 | 0.473 | 0.780 | 0.835 | 0.640 | 0.296 |
| 中文语义 | 0.414 | 0.690 | 0.793 | 0.548 | — |
| 专名精确 | 0.680 | 0.920 | 0.960 | 0.772 | — |
| 时间回放 | 0.167 | 0.417 | 0.417 | 0.306 | — |
| 关系推理 | 0.472 | 0.933 | 1.000 | 0.831 | — |
| 跨域检索 | 0.500 | 0.900 | 0.900 | 0.688 | — |
| 应召不到 | — | — | — | — | **0.571** |

**读这张表时必须知道的三件事：**

1. **`embedder` 是 `HashEmbedder`（字符 bigram 散列），没有任何语义能力。**
   工作区里**根本没有**真实 embedding 实现——`cortex-memory::embed` 只有它，
   `cortexd::AppState::new_live` 用的也是它。所以向量那一路当前实际上是
   **第二条词法通道**，不是语义通道。接上 fastembed 之后这张表要整体重跑。
2. **`R@∞` 全线 1.000 是语料规模的产物**：69 条有效事实全部塞得进 6000 token
   的注入预算，预算截断从未生效。语料上千条之后这一列才有信息量。
3. **`vector` 那一路的「覆盖 76/79」同样是规模产物**：召回宽度 40 已经是
   69 条语料的 58%，覆盖率接近抽签。有信息量的是它的 R@5 与独占数。
