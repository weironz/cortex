//! 电脑操作 —— 截屏与键鼠合成，**只在桌面端**。
//!
//! # 为什么整个模块被 `cfg` 掉一半
//!
//! Windows 与 macOS 上编真实现，别处编一个说得清的失败。这条边界正好等于
//! 产品边界：电脑操作是桌面端的能力，而这个仓库的 Linux 产物是沙箱容器与
//! agentd —— 容器里根本没有屏幕。
//!
//! 顺带解决 CI：Linux 上 `enigo` 默认要 `libxdo`（C 库）。目标平台限定让
//! Linux 连编都不编它们。代价是**Linux 桌面用不了这个功能** ——
//! 所以 [`supported`] 会照实回答，`/health` 把它报上去，界面据此决定要不要
//! 摆那个开关。摆一个打开也没用的开关，比没有这个开关更糟。
//!
//! # 坐标：模型看到的是缩小图，点击落在真实像素上
//!
//! 原图动辄 4K，直接塞进上下文要几万 token，而模型也不需要那个分辨率。
//! 所以截图按长边 [`MAX_EDGE`] 缩小，**并且这一侧记住缩放比例**
//! （[`Screen::scale`]），点击时乘回去。
//!
//! ⚠️ 反过来做（把比例告诉模型，让它自己换算）试过一次就够了：模型会算错，
//! 而算错的表现是点偏一点点 —— 它看不出是自己算错了，只会重试，然后一直偏。
//! **换算放在这一侧，模型只见一套坐标。**

/// 截图长边的上限。
///
/// 1400 是折中：够看清按钮上的字（这决定了这组工具能不能用），
/// 又不至于让一张图吃掉太多上下文。4K 原图不缩的话一张就上万 token，
/// 而一次操作要截好几张。
#[cfg(any(target_os = "windows", target_os = "macos"))]
const MAX_EDGE: u32 = 1400;

/// 这个构建 / 这台机器支持电脑操作吗。
///
/// **照实回答，别乐观**：`/health` 把它报给界面，界面据此决定摆不摆那个开关。
#[must_use]
pub const fn supported() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub use real::Computer;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub use stub::Computer;

// ───────────────────────── 真实现 ─────────────────────────

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod real {
    use super::MAX_EDGE;
    use base64::Engine as _;
    use cortex_agent::ToolResult;
    use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};

    /// 一次截屏的结果 —— 图，以及它相对真实屏幕的缩放比例。
    struct Shot {
        png: Vec<u8>,
        /// 真实像素 = 图上坐标 ÷ scale。**这一侧留着它，模型不必知道。**
        scale: f64,
        real_width: u32,
        real_height: u32,
    }

    /// 电脑操作的执行者。
    ///
    /// # 为什么每次都新建 `Enigo`，而不是存一个
    ///
    /// `Enigo` 不是 `Send`（它握着平台的输入句柄），而工具调用发生在 tokio
    /// 的任务里。存一个就得配一把锁再加一层 `spawn_blocking`，换来的只是省掉
    /// 几毫秒的初始化 —— 而这一组工具本来就是「点一下、等界面反应」的节奏。
    pub struct Computer {
        /// 上一次截屏的缩放比例。点击时用它把图上坐标换回真实像素。
        ///
        /// ⚠️ **没截过图就点，是模型的错误用法**，这里得说清楚而不是猜一个
        /// 比例蒙混过去 —— 猜错的表现是点在一个谁也不知道的地方。
        last: std::sync::Mutex<Option<f64>>,
    }

    impl Default for Computer {
        fn default() -> Self {
            Self {
                last: std::sync::Mutex::new(None),
            }
        }
    }

    impl Computer {
        /// 屏幕多大（真实像素）。提示词要用。
        pub fn screen_size() -> Option<(u32, u32)> {
            let monitor = xcap::Monitor::all().ok()?.into_iter().next()?;
            Some((monitor.width().ok()?, monitor.height().ok()?))
        }

        pub async fn call(&self, tool: &str, args: &serde_json::Value) -> ToolResult {
            match tool {
                "screenshot" => self.screenshot(),
                "click" => self.click(args),
                "type_text" => Self::type_text(args),
                "key" => Self::key(args),
                "scroll" => self.scroll(args),
                // 走到这里说明 `computer_specs` 加了一个工具而这里忘了接。
                // 说清楚，别静默成功
                other => ToolResult::err(format!(
                    "这个 agent 认得 {other} 这个名字，但没有实现它 —— 这是我们的 bug，不是你的用法问题"
                )),
            }
        }

        fn screenshot(&self) -> ToolResult {
            let shot = match Self::capture() {
                Ok(s) => s,
                Err(e) => return ToolResult::err(format!("截屏失败：{e}")),
            };
            if let Ok(mut last) = self.last.lock() {
                *last = Some(shot.scale);
            }
            let b64 = base64::engine::general_purpose::STANDARD.encode(&shot.png);
            // 文字里报的是**图上**的尺寸，与模型能用的坐标一致。
            // 报真实尺寸的话，模型会照着那个数去点，而它看到的图不是那么大
            let w = (f64::from(shot.real_width) * shot.scale).round() as u32;
            let h = (f64::from(shot.real_height) * shot.scale).round() as u32;
            ToolResult::ok(format!(
                "截好了。你看到的这张图是 {w}x{h}，图上读到的坐标可以直接用于 click / scroll。"
            ))
            .with_image(b64, "image/png")
        }

        fn capture() -> Result<Shot, String> {
            let monitor = xcap::Monitor::all()
                .map_err(|e| format!("列不出显示器：{e}"))?
                .into_iter()
                .next()
                .ok_or_else(|| "一个显示器都没有".to_string())?;
            let real_width = monitor.width().map_err(|e| e.to_string())?;
            let real_height = monitor.height().map_err(|e| e.to_string())?;
            let image = monitor
                .capture_image()
                .map_err(|e| format!("抓不到屏幕内容：{e}"))?;

            let longest = real_width.max(real_height);
            let scale = if longest > MAX_EDGE {
                f64::from(MAX_EDGE) / f64::from(longest)
            } else {
                1.0
            };
            let scaled = if (scale - 1.0).abs() < f64::EPSILON {
                image
            } else {
                let w = (f64::from(real_width) * scale).round() as u32;
                let h = (f64::from(real_height) * scale).round() as u32;
                image::imageops::resize(&image, w, h, image::imageops::FilterType::Triangle)
            };

            let mut png = std::io::Cursor::new(Vec::new());
            scaled
                .write_to(&mut png, image::ImageFormat::Png)
                .map_err(|e| format!("编码 PNG 失败：{e}"))?;
            Ok(Shot {
                png: png.into_inner(),
                scale,
                real_width,
                real_height,
            })
        }

        /// 图上坐标 → 真实像素。
        ///
        /// 没截过图就点的话**当场拒绝**，而不是按 1.0 蒙混过去：屏幕多半是
        /// 缩过的，按 1.0 会点在一个谁也不知道的地方，而模型看不出发生了什么。
        fn to_real(&self, x: i64, y: i64) -> Result<(i32, i32), String> {
            let scale = self.last.lock().ok().and_then(|s| *s).ok_or_else(|| {
                "先 screenshot 一次再点 —— 没有截图就没有可用的坐标系".to_string()
            })?;
            Ok((
                (x as f64 / scale).round() as i32,
                (y as f64 / scale).round() as i32,
            ))
        }

        fn click(&self, args: &serde_json::Value) -> ToolResult {
            let (Some(x), Some(y)) = (
                args.get("x").and_then(serde_json::Value::as_i64),
                args.get("y").and_then(serde_json::Value::as_i64),
            ) else {
                return ToolResult::err("缺少 x / y");
            };
            let (rx, ry) = match self.to_real(x, y) {
                Ok(p) => p,
                Err(e) => return ToolResult::err(e),
            };
            let button = match args.get("button").and_then(serde_json::Value::as_str) {
                Some("right") => Button::Right,
                Some("middle") => Button::Middle,
                _ => Button::Left,
            };
            let double = args
                .get("double")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);

            let mut enigo = match Enigo::new(&Settings::default()) {
                Ok(e) => e,
                Err(e) => return ToolResult::err(format!("接不上输入子系统：{e}")),
            };
            if let Err(e) = enigo.move_mouse(rx, ry, Coordinate::Abs) {
                return ToolResult::err(format!("移不动鼠标：{e}"));
            }
            for _ in 0..if double { 2 } else { 1 } {
                if let Err(e) = enigo.button(button, Direction::Click) {
                    return ToolResult::err(format!("点不下去：{e}"));
                }
            }
            // 明说「再截一张确认」：界面常常没有按预期变，而模型默认会假设它变了
            ToolResult::ok(format!(
                "点了 ({x}, {y})。再 screenshot 一张确认界面真的按预期变了。"
            ))
        }

        fn type_text(args: &serde_json::Value) -> ToolResult {
            let Some(text) = args.get("text").and_then(serde_json::Value::as_str) else {
                return ToolResult::err("缺少 text");
            };
            let mut enigo = match Enigo::new(&Settings::default()) {
                Ok(e) => e,
                Err(e) => return ToolResult::err(format!("接不上输入子系统：{e}")),
            };
            match enigo.text(text) {
                Ok(()) => ToolResult::ok(format!("输入了 {} 个字符。", text.chars().count())),
                Err(e) => ToolResult::err(format!("输入失败：{e}")),
            }
        }

        fn key(args: &serde_json::Value) -> ToolResult {
            let Some(spec) = args.get("keys").and_then(serde_json::Value::as_str) else {
                return ToolResult::err("缺少 keys");
            };
            let parts: Vec<&str> = spec
                .split('+')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            let Some((main, mods)) = parts.split_last() else {
                return ToolResult::err("keys 是空的");
            };
            let Some(main_key) = parse_key(main) else {
                return ToolResult::err(format!("不认识按键 `{main}`"));
            };
            let mut modifiers = Vec::new();
            for m in mods {
                match parse_key(m) {
                    Some(k) => modifiers.push(k),
                    None => return ToolResult::err(format!("不认识修饰键 `{m}`")),
                }
            }

            let mut enigo = match Enigo::new(&Settings::default()) {
                Ok(e) => e,
                Err(e) => return ToolResult::err(format!("接不上输入子系统：{e}")),
            };
            // 修饰键按下 → 主键点一下 → 修饰键**倒序**放开。
            // 不放开的话它们会一直按着，之后每一次输入都带上 ctrl —— 而那个
            // 状态在屏幕上完全看不出来
            for m in &modifiers {
                if let Err(e) = enigo.key(*m, Direction::Press) {
                    return ToolResult::err(format!("按不下 {m:?}：{e}"));
                }
            }
            let hit = enigo.key(main_key, Direction::Click);
            for m in modifiers.iter().rev() {
                let _ = enigo.key(*m, Direction::Release);
            }
            match hit {
                Ok(()) => ToolResult::ok(format!("按了 {spec}。")),
                Err(e) => ToolResult::err(format!("按键失败：{e}")),
            }
        }

        fn scroll(&self, args: &serde_json::Value) -> ToolResult {
            let (Some(x), Some(y)) = (
                args.get("x").and_then(serde_json::Value::as_i64),
                args.get("y").and_then(serde_json::Value::as_i64),
            ) else {
                return ToolResult::err("缺少 x / y");
            };
            let (rx, ry) = match self.to_real(x, y) {
                Ok(p) => p,
                Err(e) => return ToolResult::err(e),
            };
            let amount = args
                .get("amount")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(3) as i32;
            let (axis, length) = match args.get("direction").and_then(serde_json::Value::as_str) {
                Some("up") => (Axis::Vertical, -amount),
                Some("left") => (Axis::Horizontal, -amount),
                Some("right") => (Axis::Horizontal, amount),
                // 默认向下：滚动最常见的意图就是「往下看」
                _ => (Axis::Vertical, amount),
            };

            let mut enigo = match Enigo::new(&Settings::default()) {
                Ok(e) => e,
                Err(e) => return ToolResult::err(format!("接不上输入子系统：{e}")),
            };
            // 先把指针移过去：滚动落在**指针下面**那个窗口上，不移的话滚的是
            // 上一次鼠标停留的地方 —— 而那多半不是模型以为的那个窗口
            if let Err(e) = enigo.move_mouse(rx, ry, Coordinate::Abs) {
                return ToolResult::err(format!("移不动鼠标：{e}"));
            }
            match enigo.scroll(length, axis) {
                Ok(()) => ToolResult::ok("滚过了。再 screenshot 一张看看现在显示的是什么。"),
                Err(e) => ToolResult::err(format!("滚动失败：{e}")),
            }
        }
    }

    /// 按键名 → `enigo::Key`。
    ///
    /// 只收**常用**的那些。收不全不是问题（不认识的会得到一句「不认识按键 X」，
    /// 模型换一个说法就行）；收错才是问题 —— 把 `delete` 映射到别的键，
    /// 表现是删了不该删的东西。
    fn parse_key(name: &str) -> Option<Key> {
        let lower = name.to_ascii_lowercase();
        Some(match lower.as_str() {
            "ctrl" | "control" => Key::Control,
            "alt" | "option" => Key::Alt,
            "shift" => Key::Shift,
            "cmd" | "command" | "meta" | "super" | "win" => Key::Meta,
            "enter" | "return" => Key::Return,
            "tab" => Key::Tab,
            "esc" | "escape" => Key::Escape,
            "space" => Key::Space,
            "backspace" => Key::Backspace,
            "delete" | "del" => Key::Delete,
            "home" => Key::Home,
            "end" => Key::End,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "up" => Key::UpArrow,
            "down" => Key::DownArrow,
            "left" => Key::LeftArrow,
            "right" => Key::RightArrow,
            "f1" => Key::F1,
            "f2" => Key::F2,
            "f3" => Key::F3,
            "f4" => Key::F4,
            "f5" => Key::F5,
            "f6" => Key::F6,
            "f7" => Key::F7,
            "f8" => Key::F8,
            "f9" => Key::F9,
            "f10" => Key::F10,
            "f11" => Key::F11,
            "f12" => Key::F12,
            // 单个字符：`ctrl+c` 的那个 c
            _ => {
                let mut chars = lower.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                Key::Unicode(c)
            }
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 按键名认得出常用的那些，认不出的**报错而不是猜**。
        ///
        /// 猜错的代价不对称：认不出只是让模型换个说法，而映射错了是按下了
        /// 另一个键 —— 表现可能是删掉了不该删的东西。
        #[test]
        fn unknown_keys_are_refused_rather_than_guessed() {
            assert!(parse_key("Enter").is_some());
            assert!(parse_key("ctrl").is_some());
            assert!(parse_key("c").is_some(), "ctrl+c 里那个 c 要认得");
            assert!(
                parse_key("obliterate").is_none(),
                "不认识的键名要回 None —— 猜一个下去可能按到任何东西"
            );
        }

        /// 真的截一张。**默认不跑**（`#[ignore]`）。
        ///
        /// 它要一块真屏幕：CI 上没有，别人的 mac 上跑单测时也不该突然抓一张
        /// 他的桌面。留在这里是因为「这条路通不通」只有真跑一次才知道 ——
        /// 改完这个模块的人用
        /// `cargo test -p cortex-local --bin cortex-local -- --ignored 真的截`
        /// 验一次。
        #[test]
        #[ignore = "要一块真屏幕；改过这个模块之后手动跑一次"]
        fn 真的截得下来一张图() {
            let shot = Computer::capture().expect("这台机器上应当截得到屏");
            assert!(!shot.png.is_empty(), "截出来是空的");
            assert!(
                shot.png.starts_with(&[0x89, b'P', b'N', b'G']),
                "得是一张 PNG —— 供应商那侧按 mime 解，不是按内容猜"
            );
            assert!(shot.scale > 0.0 && shot.scale <= 1.0, "缩放比例只能是缩小");
            let long = shot.real_width.max(shot.real_height);
            let scaled_long = (f64::from(long) * shot.scale).round() as u32;
            assert!(
                scaled_long <= MAX_EDGE,
                "缩完长边还有 {scaled_long}，超过 {MAX_EDGE} —— 一张图会吃掉太多上下文"
            );
            println!(
                "屏幕 {}x{}，缩放 {:.3}，PNG {} 字节",
                shot.real_width,
                shot.real_height,
                shot.scale,
                shot.png.len()
            );
        }

        /// 没截过图就点，要当场拒绝。
        #[test]
        fn clicking_before_any_screenshot_is_refused() {
            let c = Computer::default();
            let r = c.click(&serde_json::json!({"x": 10, "y": 10}));
            assert!(!r.ok);
            assert!(
                r.content.contains("screenshot"),
                "得说清该先做什么。按 1.0 蒙混过去的话，缩过的屏幕上会点在一个\
                 谁也不知道的地方，而模型看不出发生了什么：{}",
                r.content
            );
        }
    }
}

// ───────────────────────── 别的平台 ─────────────────────────

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod stub {
    use cortex_agent::ToolResult;

    /// 编不进真实现的平台上的那一份。
    ///
    /// **它不会被调到**：`supported()` 是 false，于是这一组工具根本不进目录。
    /// 留着它是为了让上层不必到处 `cfg` —— 上层只问 `supported()`。
    #[derive(Default)]
    pub struct Computer;

    impl Computer {
        pub const fn screen_size() -> Option<(u32, u32)> {
            None
        }

        pub async fn call(&self, _tool: &str, _args: &serde_json::Value) -> ToolResult {
            ToolResult::err(
                "这个构建没有电脑操作能力（只有 Windows 与 macOS 的桌面端有）。\
                 不要假设点击已经发生。",
            )
        }
    }
}
