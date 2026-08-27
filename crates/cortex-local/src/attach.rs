//! 「这台机器接不接受远程接入」—— 一个**运行时能拨动**的开关。
//!
//! # 为什么不是启动参数就够了
//!
//! `--allow-remote-attach` 从一开始就在，但它只能从命令行传 —— 而桌面端
//! 拉起 `cortex-local` 时**从来没传过**（`local_agent_io.dart` 只传
//! bind / remote / parent-pid / addr-file）。也就是说反向隧道整条路对桌面端
//! 用户等于不存在：这个仓库榜首那个形状（「造好了没人调用」）。
//!
//! 补法有两条：
//!
//! * **开关拨动时重启 `cortex-local`。** 最省事，代价这个仓库已经付过一次 ——
//!   出站凭据从前正是靠重启换的，后果是「跑着的轮次被拦腰砍断、监听端口换
//!   一个、旧进程用退位的凭据答 401 把用户踢回登录页」（见
//!   [`crate::remote::Remote::set_token`] 那段）。为此才做了热替换，
//!   在这里重蹈它等于把刚填的坑挖回来。
//! * **热开关**（这里）：钥匙现铸/清掉、隧道起/停，进程不动。
//!
//! # 关掉必须**真的断开**，不只是不再重连
//!
//! 一个只管「以后不再拨号」的关闭是假的：那条已经建好的隧道还在服务请求，
//! 于是界面上写着关、云端照样接得进来。所以拨到关时要拆掉在飞的那条连接
//! （见 [`crate::tunnel`] 里那处 `select!`）。
//!
//! 关掉**不打断已经在跑的那一轮**：worker 上的轮次是 detached 的
//! （`runs.rs`），结果照样落进历史。断的是「再接进来」这件事。
//!
//! # 谁记着这个开关
//!
//! **不是这里。** 本进程不持久化它 —— 桌面端的 `settings.json` 是唯一的
//! 一份：启动时决定传不传 `--allow-remote-attach`，之后拨动走
//! `PUT /local/attach`。两边各存一份的话就是「配置有两份，改了一处」
//! （这个仓库 3+ 次），而症状是「我明明关了，重启又开了」。

use std::sync::{Arc, RwLock};

/// 接入开关 + 它那把钥匙。克隆代价低（全是 `Arc`）。
#[derive(Clone)]
pub struct AttachSwitch {
    /// `None` = 关。开着时是**这一次开启**现铸的那把钥匙。
    key: Arc<RwLock<Option<String>>>,
    /// 拨动过几次。**只用来叫醒隧道那个循环**，值本身没有意义。
    ///
    /// 与 [`crate::remote::Remote::token_generation`] 同款，理由也一样：
    /// 竞态落在「循环刚决定要睡 →（这里拨动）→ 才真的睡下」那个缝里，
    /// 而 `Notify::notify_waiters` 只叫醒此刻正在等的人。
    generation: Arc<tokio::sync::watch::Sender<u64>>,
}

impl Default for AttachSwitch {
    fn default() -> Self {
        Self::new(false)
    }
}

impl AttachSwitch {
    /// `on` 通常来自 `--allow-remote-attach`。
    #[must_use]
    pub fn new(on: bool) -> Self {
        Self {
            key: Arc::new(RwLock::new(on.then(mint))),
            generation: Arc::new(tokio::sync::watch::channel(0).0),
        }
    }

    /// 当前那把接入钥匙。`None` = 关着。
    #[must_use]
    pub fn key(&self) -> Option<String> {
        self.read().clone()
    }

    #[must_use]
    pub fn is_on(&self) -> bool {
        self.read().is_some()
    }

    /// 拨到开，返回这一次的钥匙。**已经开着就原样返回，不换钥匙。**
    ///
    /// 不换是有意的：换一把等于把此刻已经建好的隧道与云端名册里那份
    /// offer 一起作废，而用户按下的是一个已经是「开」的开关 ——
    /// 一次无操作不该让远端那一轮当场 401。
    pub fn turn_on(&self) -> String {
        let mut g = self.write();
        let key = g.get_or_insert_with(mint).clone();
        drop(g);
        self.bump();
        key
    }

    /// 拨到关。已经关着就什么都不做。
    pub fn turn_off(&self) {
        let mut g = self.write();
        let was = g.take();
        drop(g);
        if was.is_some() {
            self.bump();
        }
    }

    /// 订阅「开关被拨动了」。见 `crate::tunnel` 里那处 `select!`。
    #[must_use]
    pub fn generation(&self) -> tokio::sync::watch::Receiver<u64> {
        self.generation.subscribe()
    }

    fn bump(&self) {
        self.generation.send_modify(|n| *n = n.wrapping_add(1));
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Option<String>> {
        // 中毒了也要把值读出来：写路径只是换一个 `Option<String>`，
        // 不存在「被看到的中间状态」。这里因为一次无关的 panic 而返回 None，
        // 等于让接入面**静默失效**，那才是真的坏
        self.key
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Option<String>> {
        self.key
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 现铸一把，**不持久化**：钥匙的寿命就是这次「开着」的寿命，
/// 而落盘的钥匙是一件需要被保管、轮换、清理的东西。
///
/// ⚠️ **绝不复用入站凭据**（`--token`）。那把能换出站身份、绑工作区、
/// 改 MCP、开终端 —— 是「拉起我的那个桌面端」的权限。拿它当接入钥匙是
/// 最省事的写法，也正是安全不变量 2 说的那件不许做的事：controller
/// 从此持有了它。
fn mint() -> String {
    cortex_core::Id::new().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **安全不变量 3 的 worker 半边：关着就没有钥匙，于是不建隧道。**
    ///
    /// 顺带钉住不变量 2 的一半：钥匙是现铸的，不是从别处（尤其不是从入站
    /// 凭据）派生的 —— 两次开启必须给出两把不同的。
    #[test]
    fn 关着就没有接入钥匙() {
        let off = AttachSwitch::new(false);
        assert!(!off.is_on());
        assert_eq!(
            off.key(),
            None,
            concat!(
                "关着却拿得出钥匙 —— 隧道会跟着建起来，",
                "而「这个进程能跑 shell」这件事的主人从没同意过"
            )
        );

        let a = AttachSwitch::new(true).key().expect("开着就该有");
        let b = AttachSwitch::new(true).key().expect("开着就该有");
        assert_ne!(
            a, b,
            concat!(
                "两次铸出同一把钥匙 = 它是派生的而不是现铸的；",
                "一旦派生自入站凭据，controller 就等于拿到了机器主人那把"
            )
        );
        assert!(
            !a.trim().is_empty(),
            "空串钥匙会让「有没有开」这件事失去意义"
        );
    }

    /// 拨动要能被等着的人收到，**而且不许漏掉「决定睡 → 才真睡」那一下**。
    #[tokio::test]
    async fn 拨动开关会叫醒等着的隧道() {
        let sw = AttachSwitch::new(false);
        let mut seen = sw.generation();
        seen.mark_unchanged();

        // 先拨，后等 —— 用 `Notify` 写的话这一声会丢
        sw.turn_on();
        tokio::time::timeout(std::time::Duration::from_secs(1), seen.changed())
            .await
            .expect("拨在等之前，这一声不许丢")
            .expect("发送端还活着");

        seen.mark_unchanged();
        let waiter = tokio::spawn(async move { seen.changed().await });
        sw.turn_off();
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("关掉必须当场叫醒隧道 —— 否则它继续服务请求，开关是假的")
            .expect("任务没 panic")
            .expect("发送端还活着");
    }

    /// **拨动要叫醒的不止隧道 —— 心跳也在等。**
    ///
    /// 一个 `generation()` 被两个循环订阅（隧道 + 心跳），少订一处的症状
    /// 很贴脸：用户在这台机器上按下开关，转头去手机上看，那边最长 30 秒
    /// 仍然说「接不进来」。
    ///
    /// 有隧道时这条到不了（隧道一建起来 controller 自己就来拉名册），
    /// 所以它守的是**没有隧道**那条路 —— 而那正是「只在某条路上才跑的
    /// 代码，没人验就等于没写」。
    #[tokio::test]
    async fn 一次拨动能叫醒多个订阅者() {
        let sw = AttachSwitch::new(false);
        let mut tunnel = sw.generation();
        let mut heartbeat = sw.generation();
        tunnel.mark_unchanged();
        heartbeat.mark_unchanged();

        sw.turn_on();

        for (who, mut rx) in [("隧道", tunnel), ("心跳", heartbeat)] {
            tokio::time::timeout(std::time::Duration::from_secs(1), rx.changed())
                .await
                .unwrap_or_else(|_| panic!("{who}没被叫醒 —— 它会睡满自己那一轮"))
                .expect("发送端还活着");
        }
    }

    /// **重复按「开」不许换钥匙。**
    ///
    /// 换了的话，此刻已经建好的隧道与云端名册里那份 offer 一起作废 ——
    /// 而用户按下的是一个已经是「开」的开关，一次无操作不该让远端
    /// 那一轮当场 401。
    #[test]
    fn 再按一次开不换钥匙() {
        let sw = AttachSwitch::new(true);
        let first = sw.key().expect("开着");
        assert_eq!(
            sw.turn_on(),
            first,
            "重复开启换了钥匙 —— 在飞的接入会当场 401"
        );

        // 关了再开才是新的一把：旧钥匙必须作废，否则「关过」这件事没有意义
        sw.turn_off();
        let second = sw.turn_on();
        assert_ne!(
            second, first,
            "关掉再打开还给同一把钥匙 —— 那把钥匙在「关着」期间流出去过也照样能用"
        );
    }
}
