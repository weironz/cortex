//! 项目级环境：`setup.sh` 与它的镜像缓存。
//!
//! # 要解决的事
//!
//! v1 的做法是「大头预装进基镜像，用户级依赖落 `/workspace`」。够用，但
//! 每次容器重建（空闲回收之后的第一轮）都要重新 `pip install` —— 而那正是
//! 用户等得最不耐烦的时刻。
//!
//! Codex 的做法是「跑一次 setup 脚本，把容器状态缓存起来」。这里是同一个。
//!
//! # 为什么必须是两阶段，而不是「起来之后在 entrypoint 里跑」
//!
//! 这是调研推翻原方案的第三条，两份官方文档各证一半：
//!
//! - `docker commit`：*Commits do not include any data contained in mounted volumes.*
//! - `docker run --read-only`：只读 rootfs 下唯一可写的是卷与 tmpfs。
//!
//! 两条相加 ⇒ 在正式沙箱（只读 rootfs）里跑 setup 再 commit，**得到的镜像
//! 恒等于基镜像加一个空层**。而失败形态是最坏的那种：tag 照常生成、下次照常
//! 命中、setup 照常跳过、**镜像里什么都没有**。
//!
//! 附带两处：只读下 `apt` 本身就装不上；「entrypoint 里跑 docker commit」
//! 要求容器内有 docker.sock，等于交出宿主 root。
//!
//! 所以：
//!
//! 1. **setup 阶段** —— 基镜像起一次性容器，**rootfs 可写**、有网、
//!    **不发沙箱令牌**，跑 `/workspace/.cortex/setup.sh`
//! 2. **commit** —— cortexd 侧 `pause → commit → tag`，然后销毁 setup 容器
//! 3. **运行阶段** —— 用那个 tag + `--read-only` + 沙箱令牌起 cortex-local
//!
//! # hash 进 tag 名，于是失效不需要任何额外逻辑
//!
//! tag 是 `cortex-sandbox-cache:<hash>`，hash 覆盖 setup.sh 的内容 + 基镜像。
//! 改了输入就是另一个 tag，老的那个自然不再被查到 —— 没有「失效判断」这段
//! 代码，也就没有它写错的可能。

use sha2::{Digest, Sha256};

/// 缓存镜像的仓库名。GC 按这个前缀捞。
pub const CACHE_REPO: &str = "cortex-sandbox-cache";

/// 环境定义在工作区里的位置。
pub const SETUP_PATH: &str = "/workspace/.cortex/setup.sh";

/// setup 脚本的大小上限。
///
/// 64 KiB 足够任何一个正当的安装脚本。上限本身不是安全边界（脚本反正是
/// 在一个可写容器里跑的），是防「工作区里恰好有个同名的巨大文件」时
/// 把它整个读进内存。
pub const MAX_SETUP_BYTES: usize = 64 * 1024;

/// setup 阶段的 wall-clock 上限。
///
/// 10 分钟：`pip install` 一套科学计算依赖在慢网上确实要这么久。
/// 超时就当这次 setup 失败 —— **回落到基镜像**而不是让整个会话起不来。
pub const SETUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// 缓存镜像的总大小上限。超了按最久没用的先删。
///
/// 8 GiB：一个装了 node_modules 的镜像轻松上 GB，而开发机上不该被这个吃光。
pub const CACHE_BUDGET_BYTES: i64 = 8 * 1024 * 1024 * 1024;

/// 算这份环境的哈希 —— 也就是缓存镜像的 tag。
///
/// # 覆盖什么
///
/// setup.sh 的**内容**，加基镜像的标识。改任何一个就是另一个 tag。
///
/// # 为什么不把 owner 算进去
///
/// 两个用户的 setup.sh 一模一样时，他们**应该**共用一个缓存镜像 ——
/// 那个镜像里只有 `apt` / `pip` 装出来的东西，没有任何一方的数据
/// （用户数据在卷里，而 commit 不含卷）。把 owner 算进去只会让同一份
/// 依赖在磁盘上存 N 份。
///
/// 这条要写清楚，因为它看起来像个隔离漏洞而其实不是 —— 而下一个人
/// 会想「按用户分开更安全吧」，然后把缓存命中率降到零。
#[must_use]
pub fn spec_hash(setup: Option<&[u8]>, base_image: &str) -> String {
    let mut h = Sha256::new();
    h.update(base_image.as_bytes());
    h.update([0u8]); // 分隔，防「基镜像名尾部 + 脚本开头」拼出同一串
    match setup {
        Some(bytes) => h.update(bytes),
        // 没有 setup.sh 与「有一个空的 setup.sh」必须是**不同**的 hash：
        // 前者用基镜像，后者要真的跑一次（脚本可能被清空是一次有意的改动）
        None => h.update(b"<no-setup>"),
    }
    format!("{:x}", h.finalize())[..16].to_owned()
}

/// 缓存镜像的完整 tag。
#[must_use]
pub fn cache_tag(hash: &str) -> String {
    format!("{CACHE_REPO}:{hash}")
}

/// 按 LRU 挑出该删哪些缓存镜像。
///
/// 入参是 `(tag, 大小, 最后使用时间)`，返回该删的 tag。
///
/// # 为什么必须自己写这个
///
/// `docker image prune` **只清 dangling**（没有 tag 的）。我们的缓存镜像
/// 每一个都有 tag，所以 prune 一个都不会碰 —— 不自己写的话，它们只涨不减，
/// 而症状是几周后磁盘满，且看不出是谁占的。
///
/// # 为什么按「最后使用」而不是「创建时间」
///
/// 一个半年前建好、天天在用的环境镜像，按创建时间排会第一个被删 ——
/// 然后下一轮对话要重跑一次十分钟的 setup。
#[must_use]
pub fn pick_evictions(mut images: Vec<(String, i64, i64)>, budget: i64) -> Vec<String> {
    let total: i64 = images.iter().map(|(_, size, _)| *size).sum();
    if total <= budget {
        return Vec::new();
    }
    // 最久没用的排前面
    images.sort_by_key(|(_, _, last_used)| *last_used);

    let mut freed = 0;
    let mut doomed = Vec::new();
    for (tag, size, _) in images {
        if total - freed <= budget {
            break;
        }
        freed += size;
        doomed.push(tag);
    }
    doomed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 改脚本就是另一个_tag() {
        let a = spec_hash(Some(b"pip install requests"), "cortex/sandbox:dev");
        let b = spec_hash(Some(b"pip install requests\n"), "cortex/sandbox:dev");
        assert_ne!(
            a, b,
            "多一个换行也必须是另一个 tag。hash 覆盖不全的后果是**改了脚本却命中老缓存**，\
             而那不会报错 —— 用户会看到「我明明加了那个依赖，它还是说找不到」"
        );
    }

    #[test]
    fn 换基镜像也失效() {
        let a = spec_hash(Some(b"same"), "cortex/sandbox:dev");
        let b = spec_hash(Some(b"same"), "cortex/sandbox:v2");
        assert_ne!(
            a, b,
            "基镜像换了，装在它上面的东西就可能不兼容（glibc 版本、python 版本）。\
             不算进 hash 的话，升级基镜像之后所有人还在用旧的缓存层"
        );
    }

    #[test]
    fn 没有脚本与空脚本是两回事() {
        let none = spec_hash(None, "img");
        let empty = spec_hash(Some(b""), "img");
        assert_ne!(
            none, empty,
            "把脚本清空是一次**有意的改动**（「这次别装东西了」），\
             与「从来没有过脚本」应当走不同的缓存 —— \
             合并的话，清空脚本之后还在用上一次装满东西的镜像"
        );
    }

    #[test]
    fn 没超预算就一个都不删() {
        let imgs = vec![("a".into(), 100, 1), ("b".into(), 100, 2)];
        assert!(
            pick_evictions(imgs, 500).is_empty(),
            "总量没超就不该删。删镜像意味着下次要重跑十分钟的 setup"
        );
    }

    #[test]
    fn 按最久没用的删而不是按最早建的() {
        // b 建得最早（这里不体现），但**最近还在用**；a 很久没用了
        let imgs = vec![
            ("最久没用".into(), 300, 10),
            ("昨天还在用".into(), 300, 100),
            ("刚用过".into(), 300, 1000),
        ];
        let doomed = pick_evictions(imgs, 700);
        assert_eq!(
            doomed,
            vec!["最久没用".to_owned()],
            "只该删到刚好不超预算，且从最久没用的开始。\
             按创建时间排的话，一个天天在用的老环境会第一个被删掉"
        );
    }

    #[test]
    fn 超得多就删够为止() {
        let imgs = vec![
            ("t1".into(), 300, 1),
            ("t2".into(), 300, 2),
            ("t3".into(), 300, 3),
        ];
        let doomed = pick_evictions(imgs, 350);
        assert_eq!(
            doomed,
            vec!["t1".to_owned(), "t2".to_owned()],
            "900 要降到 350，删两个（剩 300）就够了。删三个是多删了一个，\
             而每多删一个就是下次多一次十分钟的 setup"
        );
    }
}
