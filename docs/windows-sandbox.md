# Windows 沙箱：两个后端、它们各自的取舍，以及一路踩的坑

> 本机（桌面端）那一支。云端容器沙箱在 [sandbox.md](sandbox.md)，是另一件事。

这份文档记录 2026-08-27～28 那两天做出来的东西：**结论**（采纳了什么、为什么）、
**实测数据**（每个数字都是在真机上量的），以及**踩过的坑**——最后这部分是重点，
因为其中有一半是「看起来做完了、其实什么都没验到」那种，光看代码看不出来。

---

## 零、在这之前，Windows 上是怎么跑的

Windows 没有 landlock / seatbelt 的对应物，所以 `capability()` 一直报「不可用」。
而 `sandbox::prepare` 对「不可用」有**两条独立的放行理由**：

```rust
let reason = if policy.attended.is_attended() {
    "有人在场：本机无沙箱，本次执行由用户当场批准"
} else if degraded_allowed() {
    "显式降级：本机无沙箱，已按环境变量放行"
} else {
    return Err(... "拒绝执行：本机没有可用的进程级沙箱" ...)
};
```

| | 之前的行为 |
|---|---|
| 桌面端 | 走「有人在场」——`cortex-local` 开 `.attended()`。命令**照跑，完全没有 OS 沙箱**，唯一的闸是「你逐条点允许」，结果里印一行 `⚠ 本次执行没有 OS 沙箱保护` |
| 云端 cortexd | 一律 `Attended::No`（批准的人可能在另一个城市），Windows 上硬拒。云沙箱本来就是 Linux 容器 + landlock，不受影响 |
| 文件工具 | `read_file` / `write_file` / `list_dir` **从不依赖 OS 沙箱**（`ToolSandbox::resolve` 是纯路径逻辑），一直正常 |

**为什么必须补上**：「远程接入」上线后，远程接进来的一方**能自己按那个确认**——
于是「Windows + 远程接入」等于把一条无沙箱的执行通道交出去。这是这一整轮的起点。

---

## 一、结论：两个后端，两种取舍，不是强弱关系

| | **AppContainer**（默认） | **受限令牌**（`CORTEX_WIN_BACKEND=restricted`） |
|---|---|---|
| 读 | **默认拒绝**（allow-list，强） | **不设界**——读身份就是用户本人 |
| 写 | 只有工作区 | 只有工作区（`WRITE_RESTRICTED` + 只授工作区的 logon SID） |
| 网络 | capability 控制，**默认断** | 未隔离（明文出得去） |
| 桌面 | 未隔离 | 私有桌面（挡屏幕抓取 / 输入注入） |
| `cargo build` 出 .exe | ❌ 链接步挂 | ✅ |
| `git` clone/fetch/push | ⚠️ 受 8.3 与卷根限制 | ✅ |
| `dir` / `vol` | ❌ 要卷根 ACE（一次管理员） | ✅ |
| `cargo` 拉**新**依赖 | ❌ | ✅（走本机明文回环镜像，见 4.4） |
| `curl https://` | ✅ | ✅（沙箱备一份 LibreSSL 版 curl，见 4.5） |
| 管理员 | 不要 | 不要 |

**用途因此是分开的**：

- 「关住不受信代码、防它偷读」→ AppContainer。代价：跑不了完整构建。
- 「防误写 / 防篡改地跑我信得过的工具链」→ 受限令牌。代价：不挡读。

**两者不可兼得，这是 Windows 原生沙箱的结构现实**，不是实现没做完。业界各弃一角：
Anthropic / Cursor / VS Code 弃原生 Windows（退 WSL2），OpenAI codex 二选一，
微软自己撞同一堵墙、在造 25H2+ 的新原语。

代码：`crates/cortex-agent/src/sandbox/windows.rs`（AppContainer，约 1000 行）与
`windows_restricted.rs`（受限令牌，约 550 行，机制搬自 codex 的 Apache-2.0 实现）。
逃逸测试：`tests/sandbox_escape_windows.rs`（9 条）与
`tests/sandbox_restricted_windows.rs`（5 条），两份都进了 CI 的 windows 腿。

---

## 二、选型：为什么不是别的路

| 路线 | 判决 | 理由（都是实测或源码级证据） |
|---|---|---|
| 受限令牌**单独**用 | 不够 | 裸受限令牌起 `node` / `powershell` / `link.exe` 全 `0xC0000142`。配齐四件套才可用（见下） |
| codex 的**独立本地用户** | 否 | 「一次管理员」是假的：`SETUP_VERSION` 每次 bump / 代理配置变 / 登录修复都再弹 UAC（codex 自己已 bump 到 5）。唯一免弹的预置通道要求**安装器提权**，而我们的安装器是 `PrivilegesRequired=lowest`，静默自动更新在结构上依赖它。另外 20k 行 fork + 每台机器留隐藏用户 / HKLM Winlogon / WFP 状态；无签名 exe 建隐藏账户还是恶意软件指纹 |
| `subst` / junction 路径重映射 | 否 | 模型看到的路径不再是真路径，会漏进 pwd、编译产物、panic 栈、commit message，**永久进 git 历史**。而且 LowBox 下 subst 能否解析零实测 |
| WSL2 | 否（对这个用途） | 在 WSL2 里 cargo 编的是 **Linux 二进制**，而 Windows 项目要 MSVC 工具链。它是「Linux 目标开发」的答案，不是「在沙箱里构建 Windows 项目」的答案 |
| 关掉 8.3 短名（`fsutil`） | 否 | 要管理员、是改整卷的破坏性动作，**而且没用**——`GetLongPathNameW` 的判据是名字形状，不是短名是否存在 |
| git 侧规避 getcwd | 否 | 源码级判死：`setup.c` 无条件 `getcwd`，`mingw_getcwd` 没有任何旋钮 |
| 等微软新原语 | 否 | BaseContainer / CreateProcessInSandbox / BFS 全建在 AppContainer 之上，能力位枚举里没有任何路径解析相关项。微软自己遇到同类问题的答案是**换原语**（WSL 容器 / Isolation Session），不是修 |

---

## 三、AppContainer 后端：机制与实测

### 3.1 为什么要一个 helper 进程

AppContainer 靠 `STARTUPINFOEXW` 上的
`PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` 生效，而 **`std::process::Command`
在 stable Rust 上设不了 proc-thread 属性**。所以 `prepare` 回的不是真命令，是
「本程序 + `--win-sandbox-exec` + 一段 JSON」，由 helper 用 `CreateProcessW` 起真命令。
stdio 靠继承，不必自己铺管道。

### 3.2 授权的代价（实测）

访问检查要求对象 DACL 上有容器 SID 的 ACE；带继承标志写 DACL 会铺满整棵已有子树。

| | 耗时 |
|---|---|
| 能力探测（进程内一次） | 61 ms |
| 空工作区，每条命令 | **60 ms** |
| 793 059 个文件的仓库，**首次**授权 | **153.8 s** |
| 同一个仓库，之后每条命令 | **201 ms** |

`target/` 占了 793k 里的 762k——沙箱建出来的文件是**创建时继承**的，不要钱；
贵的只有「绑定一个已经建过的大仓库」这一种情形。

### 3.3 网络是一个 capability 的事（实测）

| | `curl -m 8 https://example.com` | DNS |
|---|---|---|
| 不给 capability | 退出码 6 | 失败 |
| 给 `internetClient`（`S-1-15-3-1`） | **200** | 通 |

⚠️ **两边的 `Allowed` 不是同一个东西**：Linux 侧走 `cortex-egress-proxy` 的 CONNECT
白名单，这边一放就是整个互联网。loopback 始终断着（AppContainer 的老规矩），
所以沙箱里连不上本机的开发服务器。

---

## 四、受限令牌后端：机制与实测

### 4.1 令牌

`CreateRestrictedToken(DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED)`：
**写**检查要求既过普通 DACL、又匹配一个 restricting SID；**读**只走普通 DACL。

`restricting SIDs = [logon SID, Everyone]`：

- **logon SID 是写作用域的锚**——父进程只把**工作区**授给它，于是写只落工作区。
- **Everyone 是不得不放的**：`link.exe`（带 `/DEBUG`）要在会话的 `BaseNamedObjects`
  里建命名对象跑 mspdbsrv IPC，那里授的是 Everyone。去掉它 `cargo build` 的链接步
  当场挂（实测）。代价：Everyone 可写的目录也写得进。

### 4.2 「四件套」里真正管用的是哪一件

裸受限令牌起 `node` / `powershell` / `link.exe` 会 `0xC0000142`
（`STATUS_DLL_INIT_FAILED`）。codex 用四件套解掉，我们照搬——但**故障注入把它分清了**：

| | 作用 | 去掉会怎样 |
|---|---|---|
| **令牌默认 DACL** = `[logon, Everyone]` 全权 | 进程建的管道 / section 对象自己够得着 | **`cargo build` 立刻挂** ← 真正修 `0xC0000142` 的是它 |
| 私有桌面 + 给 logon SID 授桌面权 | **隔离**（挡屏幕抓取 / 输入注入） | 不影响启动 |
| `STARTUPINFOW.lpDesktop` 指向它 | 同上 | 同上 |
| 重开 `SeChangeNotifyPrivilege` | `DISABLE_MAX_PRIVILEGE` 把 bypass-traverse 一起剥了 | 穿目录到处要显式授 |

**不要照抄「四件套都是必需」**——那是没验过的说法。

### 4.3 `%TEMP%` 要重定向

真 `%TEMP%` 授的是用户本人、不是 logon SID，写不进，而 rustc / link 到处用它。
helper 在起进程前把 `TMP` / `TEMP` 指进工作区内。

### 4.4 cargo 要两样东西：自己的 `CARGO_HOME`，和一个明文回环镜像

**（1）`CARGO_HOME` 必须换掉。** 用户真实的 `~/.cargo` 沙箱写不进（写只授了
工作区），而 cargo 第一件事就是往 `registry/index/…` 里建目录。**这一步比 TLS
更靠前**：实测哪怕依赖全在缓存里，默认 `CARGO_HOME` 下也当场

```
failed to create directory `C:\Users\…\.cargo\registry\index\…`
拒绝访问。 (os error 5)
```

所以这一档把 `CARGO_HOME` 指到 `%LOCALAPPDATA%\Cortex\win-sandbox\cargo-home`，
授 **Everyone**（不是 logon SID —— 这个目录跨登录会话长期存在，按 logon SID
授会每登录一次攒一条永不再匹配的 ACE）。

⚠️ **「Everyone 在 `%LOCALAPPDATA%` 底下等于只有本人」这句话是有条件的**，
而条件在这台机器上就不完全成立：实测 `%LOCALAPPDATA%` 上带着一条
`CodexSandboxUsers:(I)(OI)(CI)(RX)`（用户装的 codex 建的本地组），同机另一套
沙箱够得到这里，而这条 Everyone 让它还写得进。兜底是 cargo 自己的校验和 ——
`.crate` 的 sha256 来自索引、有锁文件时来自锁文件，塞进来的东西过不了那一关。
选它是因为另外两条更差：授用户真实的 `~/.cargo` 等于让沙箱污染**沙箱外**的
构建（真正的逃逸），按 logon SID 授则每次登录攒一条死 ACE。

**为什么不干脆把 `~/.cargo` 授给沙箱写**：沙箱里的代码就能往用户的 crate 缓存
里塞东西，而**下一次不在沙箱里的构建会照单编译它**。那是一条真正的逃逸路径。

代价如实写在工具描述里：第一次构建会重新下载依赖，之后各工作区共用这一份。

**（2）`.crate` 的下载走本机镜像。** 受限令牌下 schannel 建不出 TLS 客户端凭据
（5.10），而 cargo 把 libcurl 静态链进去且只编了 Schannel，**没有任何开关**。
唯一剩下的形状是让 cargo 那一侧只说明文：宿主进程（未受限，TLS 正常）在回环上
开一个镜像，沙箱里的 cargo 用 `source.crates-io.replace-with` 指过来。

代码在 `crates/cortex-agent/src/sandbox/windows_cargo_mirror.rs`。三条路由：
`/index/config.json`（合成）、`/index/*`（转发 index.crates.io）、
`/dl/<name>/<ver>/download`（转发 static.crates.io）。

| 设计点 | 为什么 |
|---|---|
| **完整性没变弱** | cargo 校验 `.crate` 的 sha256，校验值来自索引；索引与文件都由镜像原样转发。有 `Cargo.lock` 时校验值来自锁文件，是端到端的 |
| **固定端口 47823** | source replacement **只能写配置文件，没有环境变量**（实测：`CARGO_SOURCE_CRATES_IO_REPLACE_WITH` 被 cargo 完全无视）。配置是所有沙箱命令共用的一份，URL 必须恒定 |
| **端口被占时先验明正身** | 打一次 `/cortex-mirror` 看标记。是自己人就复用（镜像无状态，谁起的都一样）；不是就**不写那段配置** —— 宁可让 cargo 报原来的错，也不能把它指到来路不明的 registry 上 |
| **镜像开在父进程** | helper 每条命令起一次；让它持端口的话，先结束的那条会把还在下载的那条掐断 |

### 4.5 `curl` 换成 LibreSSL 版 —— 与 git 的修法同形状

系统的 curl 和 Git 自带的 curl **都只编了 Schannel**（版本串一模一样），没有
运行时开关。而 curl 官方的 Windows 构建（curl.se/windows，curl-for-win 项目）
用的是 **LibreSSL**，不碰证书存储 —— 实测在受限令牌沙箱里 HTTPS 全通。

代码在 `crates/cortex-agent/src/sandbox/windows_curl.rs`：首次使用时下载
（URL 与 **SHA-256 钉死在源码里**，哈希不符拒绝解压）、解到
`%LOCALAPPDATA%\Cortex\win-sandbox\tools\`，之后每条命令注入 PATH 前缀 +
`CURL_CA_BUNDLE`。

两个非显然点：

- **`CURL_CA_BUNDLE` 必须一起给。** 这份构建带 NativeCA 特性，默认去读
  Windows 证书存储做验证 —— 沙箱里握手能过、验证挂在 unable to get local
  issuer 上。指到随包的 `curl-ca-bundle.crt` 才全通。
- **下载、算哈希、解压全用系统自带工具，按绝对路径调**（`System32` 的
  curl / certutil / tar）。裸敲 `tar` 在 Git Bash 环境里解析到 GNU tar，
  它不认 zip（实测踩过）。

⚠️ 维护成本如实写下：curl.se 只保留最近几个版本的下载目录，上游发新版后
钉死的 URL 会 404。那时下载失败 → WARN → 不注入 PATH → curl 退回 Schannel
的老报错（诚实回落），逃逸测试那条 `curl 走 https 能通` 会红，提醒同时换
URL 和哈希。

这条修不了 PowerShell：`Invoke-WebRequest` / .NET 的 HTTPS 走 SSPI，
没有可换的后端 —— 工具描述里让模型用 `curl` / `git` 代替。

---

## 五、坑（重点）

### 5.1 每条命令 +23.4 秒，而且在偷偷放宽宿主机权限

`SandboxPolicy` 无条件把 `~/.cargo/registry`、`~/.cache`、`~/.gradle/caches` 放进
可写根——Landlock 下那不要钱（授权是进程的、随进程消失），Windows 上每一个都是
一次**持久的、递归的 ACL 重写**：`.cargo/registry` 6.97 s、`.gradle/caches` 5.01 s、
`.cache` 3.61 s、`.bun` 1.00 s，**合计 23.4 秒，每条命令一次**。

而慢还是次要的：它把「容器可写」**永久刻进**了用户真实的 cargo 仓库，沙箱退出之后
还留着。**一个为了限制而存在的机制，反过来在宿主机上放宽了权限。**

修法：只授工作区子树 + 授过不再授 + 授权挪到父进程（helper 跑在命令超时**里面**，
放那儿的话大仓库上第一条命令会以「命令超时」收场，把「正在准备工作区」谎报成
「你的命令有问题」）。

### 5.2 逃逸测试全绿，但什么都没验（**两层，各自都很合理**）

**第一层：跑的是旧二进制。** helper 默认解析到 `target/debug/cortex-local.exe`，而
`cargo test -p cortex-agent` **不重编另一个 crate 的 bin**。把 AppContainer 属性整个
摘掉，四条逃逸测试**照样全绿**。

试过用修改时间比新旧——不成立：`cargo test` 总是**最后**重链测试二进制，它永远比那个
bin 新。改成根除而不是检测：测试目标设 `harness = false`，**测试二进制自己就是 helper**。

**第二层：修好之后仍然全绿。** 因为 `command_line` 把引号按 `CommandLineToArgvW`
的规则转义，而 **cmd.exe 不认那套**——命令根本没跑起来。

这第二层还揪出一个**先存在的产品 bug**：`std::process::Command` 对 `cmd.exe /C` 用
同一套转义，所以 **Windows 上任何带引号路径的命令一直是坏的**
（`type "C:\Program Files\x\a.txt"` → 「文件名、目录名或卷标语法不正确」），
沙箱内外、hook 里都一样。而带空格的路径在 Windows 上是常态。

> **通用自查**：把被测的保护**整个关掉**，测试必须红。这一步比写测试本身重要——
> 它同时验了「测试在观察什么」和「被测的东西真的跑了」。

### 5.3 判断的语义被脚下换掉

Linux 那份逃逸测试的跳过判据是「本机没有沙箱能力」。Windows 上一直靠它顺带跳过——
接上 AppContainer 之后那个巧合没了，**七条当场全红**。判据补上 `cfg!(unix)`。

同一形状的第二处：`attended_execution.rs` 那组守着「没有沙箱时怎么办」，判据是
`if capability().is_available() { 跳过 }`。三个平台都有沙箱之后，**它们在哪儿都不跑了**，
而没有任何东西变红。改成 `sandbox::prepare_with(cap, ...)` 让测试把那个世界直接构造出来。

### 5.4 8.3 短名——让所有观察自洽的那条规则

`GetLongPathNameW` 的名字查找**不是每一级都做**：一个组件的名字如果不可能是 8.3
短名（太长、带连字符），它就不去父目录里查，那一级于是不需要列举权。

| 路径 | 卷根未授权时 |
|---|---|
| `D:\cortex-probe\ws` | ✅ 通（`cortex-probe` 12 字符，不可能是短名） |
| `D:\codes\cortex` | ❌ 挡（`codes` 是 8.3 形状，必须去 `D:\` 里查） |

「`C:\Users\willz` 授上了还是不行」也是同一条：`willz` 五个字符，要去 `C:\Users` 里查，
而那一级归 SYSTEM 改不动。**真实项目目录的名字大多是短的，所以实践中卷根那条 ACE
多半还是要的**——而它需要管理员（实测连不做传播的 `SetFileSecurityW` 对 `C:\`、`D:\`
也回 `ERROR_ACCESS_DENIED`，是 `WRITE_DAC` 的问题，不是传播的锅）。

### 5.5 链断了还往下授 = 白付代价

祖先链是一条**链**：中间少一级，`GetLongPathNameW` 整条失败，下面授得再多也没用。
第一版是「授不上就跳过、继续往下」，于是在 `C:\Users` 授不上的机器上（那是常态），
**主目录被加上了列举权**——`.ssh`、`.git-credentials` 的文件名全露出来，而 git 依然
一步都走不了。**不是白干，是白付代价。** 改成一断即止，有测试守着。

### 5.6 换个 API 快五个数量级——但反过来不成立

`SetNamedSecurityInfoW` 会做自动继承传播（把可继承 ACE 铺到每个子项）。祖先目录那些
ACE 是**不继承**的，子项什么都不会变，那趟遍历纯属白跑。

| 797 381 个文件的目录，加一条不继承的 ACE | 耗时 |
|---|---|
| `SetNamedSecurityInfoW` | **198.9 s** |
| `SetFileSecurityW`（被它取代的老接口，不做传播） | **0.38 ms** |

⚠️ **反过来不成立**：老接口**不能**用来撤销可继承的 ACE——根上撤了、子项还带着继承
来的那条，留下「根没权限而子树有权限」的不一致状态，**而且没有任何症状**。实测踩过。

验过的：写回之后原先带 `(I)` 的条目还带着 `(I)`（没被压扁成显式条目）。
**没验的**：`SE_DACL_AUTO_INHERITED` 那一位观察不到（`icacls` 不显示，`Get-Acl` 露的是
另一位），删掉置位那行测试照样绿——留着是便宜的保险，但别以为它被验过了。

### 5.7 canonicalize 挂 ≠ cargo 跑不起来

AppContainer 里 `std::fs::canonicalize` 回 `os error 5`
（`GetFinalPathNameByHandleW(VOLUME_NAME_DOS)` 被对象管理器层拒绝，**文件 ACL 够不着**）。

我据此断言「AppContainer 跑不了 cargo」——**错了**。cargo / rustc 从 **1.70（2023-06）**
起全部改走 `try_canonicalize`，失败即回落纯词法路径（`GetFullPathNameW`），核心工具链
不再需要 `VOLUME_NAME_DOS`。实测：`cargo check` / clippy / rustc **全通**。

**教训**：「某个底层 API 失败」不等于「用它的工具挂了」。要测真实工作负载。

### 5.8 AppContainer 真正跑不了的是**链接**

`cargo build` 挂在最后一步：

- MSVC `link.exe` → `0xC0000142`
- `rust-lld` → 从 `.rustup`（主目录树）跑报 `permission denied`，**从工作区拷一份跑却完全正常**
  （FULL 授权也没救）——差别是**位置**：LLD 要 mmap 自己的镜像和 sysroot 库（创建 section），
  而 AppContainer 拒绝从那些位置创建 section。

这是 structural 的，不是 ACL 能调的。**这才是做第二个后端的直接原因。**

### 5.9 受限令牌下没法只对沙箱拒读——我拿用户的 `~/.ssh` 和 `~/.docker` 证明了

想给秘密目录下 DENY-READ 来补上读边界。但 `WRITE_RESTRICTED` 只让 restricting SID
参与**写**检查，**读**走普通令牌——沙箱进程的读身份就是**用户本人**，
**没有能区分「沙箱 vs 用户」的 SID**。

于是我只能拿 `Everyone` 下 DENY。而 `Everyone` 包含用户自己：

- **`~/.ssh`** 被 `Everyone:(OI)(CI)(DENY)(RX)` 挡住，用户读不了自己的私钥。
- **`~/.docker`** 同样，Docker Desktop 挂了，**另一个并行会话被迫去「夺回所有权 + 重置继承权限」**。

那段代码整个删了，不留「看起来在挡、其实在误伤」的东西。这一档**不挡读**，
且这一点写进了模块文档、`shell` 工具描述与能力上报，有测试守着。

> **清理时的坑**：不带 `/T` 的 `icacls /remove` **不传播**，父目录看着干净了，
> 子项还留着继承来的 ACE。收尾要连子项一起查（`icacls dir\*`）。

### 5.10 HTTPS 全挂——根因是证书存储只能只读打开

受限令牌下 `git clone` / `cargo` 拉依赖 / `curl https` 全报
`schannel: AcquireCredentialsHandle failed: SEC_E_NO_CREDENTIALS`，**而 HTTP 明文照通**
——既不是安全边界，又坏了可用性。

写了个最小 SSPI 复现，沙箱内外各跑一次二分出根因：

| | 沙箱外 | 受限令牌沙箱内 |
|---|---|---|
| `CertOpenSystemStore(ROOT/MY/CA)`（读写） | OK | **ACCESS_DENIED** |
| `CertOpenStore(..., CERT_STORE_READONLY_FLAG)` | OK | **OK** |
| `CryptAcquireContext(VERIFYCONTEXT)` | OK | OK |
| `AcquireCredentialsHandle(schannel)` | OK | `SEC_E_NO_CREDENTIALS` |

**证书存储只能只读打开**，而 schannel 内部按读写开。这不是我们漏授——那个注册表键上
明写着 `NT AUTHORITY\RESTRICTED: ReadKey`，是 **Windows 对受限令牌的既定语义**。

**修法：不去要写权限，让程序绕开 schannel。**

| | 现状 |
|---|---|
| `git`（clone / fetch / push / ls-remote） | ✅ 通——注入 `http.sslBackend=openssl`，走 git 自带 ca-bundle |
| `cargo` 更新索引 | ✅ 通——`CARGO_NET_GIT_FETCH_WITH_CLI` + 索引走 git 协议 |
| `cargo` 下载 `.crate` | ✅ 通——**绕开 TLS**：走本机的明文回环镜像（4.4） |
| `curl https://` | ✅ 通——沙箱备一份 LibreSSL 版 curl（4.5） |
| PowerShell `Invoke-WebRequest` / .NET | ❌ 挂——SSPI，没有可换的后端 |

规律：**能换掉 TLS 后端、或能被换成明文的都修好了；剩下的是自己链了 Schannel
又没有替代通道的那些**。给它们 CA 文件（`--cacert` / `CARGO_HTTP_CAINFO`）没用——
那只换验证用的根，不换 TLS 后端；而且失败发生在**建凭据**，比验证更靠前，
所以本地 MITM 代理 + 自签 CA 一样没戏。

（先猜过「把用户 SID 加进令牌默认 DACL」，没用，已回滚。**猜不如量。**）

### 5.11 零管理员做不到真封网

| 机制 | 非提权结果 |
|---|---|
| **WFP**（codex 用的按 SID 过滤） | `FwpmEngineOpen0` OK，但 **`FwpmTransactionBegin0` 回 `ERROR_ACCESS_DENIED`**（只读） |
| Job object 网络限速 | **不可设**（err 87） |

codex 的 WFP 封锁也**只在提权档**；它非提权档的「断网」就是设 `HTTP_PROXY` 等环境变量
——**劝退，不是封锁**。

### 5.12 fork bomb：测试二进制既是 helper 又是主体

受限令牌的逃逸测试只拦了 `--win-restricted-exec`，而 `capability()` 探测 AppContainer 时会
spawn `self --win-sandbox-exec`——那个子进程没被拦就重跑整个测试 `main`，每次又触发一次
`capability()` → 再 spawn，**指数级自我复制，一度到约 1.3 万个进程**。

`harness = false` 的测试二进制**必须拦截所有 helper flag**。生产的 `cortex-local` 两个都接了，
从不在此风险内。

### 5.13 并行会话共用一台机器

这台机器上同时有多个会话在干活。**改机器全局状态就是在改别人的运行环境**：

- 上面 5.9 的 DENY 让另一个会话被迫去修 Docker。
- `taskkill //IM cortex-local.exe` 按镜像名杀，把用户桌面端的 agent 一起杀了。
- fork bomb 让全机变慢。

**规矩**：只授权不拒绝；杀进程只杀自己造的、名字唯一的；改用户主目录或盘符根的 ACL、
动 docker / 全局服务之前先说一声。

### 5.14 测试替被测对象把环境铺好了

`cargo build 出 .exe` 那条测试从第一天起就绿，而它的命令是：

```
set "CARGO_HOME=%CD%\.cargo-home" & cargo build
```

那一句是**测试自己加的**。真实用户的项目里没有它，于是 cargo 走默认
`CARGO_HOME`，第一步就死在「拒绝访问 (os error 5)」——比 TLS 更靠前的一堵墙，
而测试从来没撞到过。文档、`shell` 工具描述、roadmap 三处都据此写着
「这一档能 `cargo build`」，**说得到做不到**（CLAUDE.md 硬约束第 2 条）。

自查：**测试里每一句「为了让它跑起来」而加的环境设置，都要问一遍
「用户那边谁来加这一句」。** 没人加，那这条测试验的就是另一个世界。

### 5.15 判据落在了另一支上，而那一支也有断言

`windows_上_shell_的描述要跟着后端翻面` 是守硬约束第 2 条的那条测试。它按
`capability()` 分三支断言：受限令牌 / AppContainer / 无沙箱。

而 `cargo test --lib` 里 helper 二进制（`cortex-local.exe`）在那个 target 目录
下**不存在**，探测于是报「不可用」——测试一路落到「无沙箱」那一支，两个
Windows 后端的断言**一条都没跑过**。把描述里的关键句整个删掉，它照样绿。

这与 5.3 是同一族，但更难发现：5.3 那两处是**跳过**，至少还能从「跑了几条」
看出来；这里是**走了另一支，而那一支也有断言、也通过**，输出里一切正常。

修法是把判据从环境里拿走：描述改成能力的**纯函数**
（`shell_description_for(&cap)`），测试自己构造三种能力，一次运行验三支。

> **通用自查**：`match 环境 { ... }` 形状的测试，问一句「本机此刻走的是哪一支，
> 另外几支谁来跑」。答案通常是「没人」。

---

## 六、边界之外：明确决定**不做**的（2026-08-28 拍板）

主线（两档可用、边界如实描述、CI 守着）闭环之后，剩下的每一件都过了一遍
「值不值」，**拍板全部不做**。列在这里是为了下一个会话别把它们当欠账捡起来
—— 要重启哪一件，先推翻对应那格的理由。

| | 不做什么 | 为什么不做 |
|---|---|---|
| a | AppContainer 档的 `dir` / `vol`，以及短名目录下的 `git` | 要**卷根一条 ACE**（一次管理员），而「不要管理员」正是当初选 AppContainer 的理由。工具描述已教模型绕开（`list_dir` / `for %f`），要完整工具链就切受限令牌档 |
| b | PowerShell `Invoke-WebRequest` / .NET 的 HTTPS | SSPI 没有可换的后端，**没有同形状的修法**。curl 已用「换一份二进制」修掉（4.5），.NET 没有对应物 —— 模型被告知用 `curl` / `git` 代替 |
| c | 真封网 | 零管理员做不到（5.11），一次管理员又违背默认档的立身之本。需要出网控制的场景交给云沙箱（那边有 `cortex-egress-proxy` 白名单） |
| d | 受限令牌档的读边界 | 机制上做不到（5.9）。要强读边界就用 AppContainer 档 —— 这正是保留两档的理由 |
| e | 桌面隔离（AppContainer 档） | 受限令牌档有私有桌面；AppContainer 与私有桌面的组合没实测过，收益（挡屏幕抓取）对这一档的典型用途（check / lint / 分析）不值再一轮踩坑 |

---

## 七、改这块代码之前要知道的

1. **CI 跑不到大部分东西**：`ci.yml` 的 windows-latest 腿只显式跑
   `--lib sandbox` + 两份逃逸测试。改 `windows*.rs` 之后**必须本机跑一遍**。
2. **两份逃逸测试都是 `harness = false`**——测试二进制自己就是 helper，
   所以验的必然是刚编出来的代码。加新测试时记得拦截**所有** helper flag（5.12）。
3. **任何新的边界断言，都要做故障注入**：把被测的保护关掉，它必须红。
   这一整轮里有三条测试是「写完看着对、一注入才发现什么都没验」。
4. **能力描述必须跟着后端翻面**（CLAUDE.md 约束 2）：`capability()` / `status_line()` /
   `shell` 工具描述三处都按后端分流，有测试守着。说错了后端，模型与用户都会
   以为自己在另一个边界里。
