// 取件自 goose（https://github.com/block/goose），Apache-2.0，
// Copyright Block, Inc. 见仓库根目录的 NOTICE。
//
// **这份代码此后由 Cortex 自行维护，不再与上游同步。** 改它按本仓库的
// 判据来，不必去看上游怎么写 —— 那边的分支已经不是我们这份的未来。
use std::{borrow::Cow, io::Read as _, path::Path};

use base64::Engine as _;
use rmcp::model::{AnnotateAble as _, ImageContent, RawImageContent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::errors::ProviderError;

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub enum ImageFormat {
    OpenAi,
    Anthropic,
}

/// Convert an image content into an image json based on format
pub fn convert_image(image: &ImageContent, image_format: &ImageFormat) -> Value {
    match image_format {
        ImageFormat::OpenAi => json!({
            "type": "image_url",
            "image_url": {
                "url": format!("data:{};base64,{}", image.mime_type, image.data)
            }
        }),
        ImageFormat::Anthropic => json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": image.mime_type,
                "data": image.data,
            }
        }),
    }
}

pub fn detect_image_path(text: &str) -> Option<Cow<'_, str>> {
    const EXTENSIONS: [&str; 3] = [".png", ".jpg", ".jpeg"];
    const MAX_PATH_LEN: usize = 4096;

    let mut best: Option<(usize, Cow<'_, str>)> = None;
    let mut from = 0;
    while from < text.len() {
        let Some(end) = EXTENSIONS
            .iter()
            .filter_map(|ext| find_ascii_ci(text, ext, from).map(|i| i + ext.len()))
            .min()
        else {
            break;
        };

        let terminator = text.get(end..).and_then(|rest| rest.chars().next());
        let terminated = terminator.is_none_or(is_path_terminator);

        if terminated {
            let mut floor = end.saturating_sub(MAX_PATH_LEN);
            while floor < end && !text.is_char_boundary(floor) {
                floor += 1;
            }
            if let Some(window) = text.get(floor..end) {
                for rel in candidate_anchors(window) {
                    let start = floor + rel;
                    let preceded_by_boundary = text
                        .get(..start)
                        .and_then(|prefix| prefix.chars().next_back())
                        .is_none_or(is_path_leading_boundary);
                    if !preceded_by_boundary {
                        continue;
                    }
                    let Some(candidate) = text.get(start..end) else {
                        continue;
                    };
                    if let Some(candidate_path) = image_path_candidate(candidate) {
                        // Keep the first referenced path, but allow a longer
                        // match anchored at the same start to extend it (a
                        // whitespace-terminated extension may be a prefix of a
                        // spaced filename ending in a later extension).
                        match best {
                            Some((best_start, _)) if start == best_start => {
                                best = Some((start, candidate_path));
                            }
                            None => best = Some((start, candidate_path)),
                            Some(_) => {}
                        }
                        break;
                    }
                }
            }
        }
        from = end;
    }
    best.map(|(_, candidate)| candidate)
}

/// 一段文本里，绝对路径**可能从哪里开始**。
///
/// ══════════════════════════════════════════════════════════
///  与上游的分歧（Cortex 自己加的，2026-08-19）
///
///  上游只找 `/`，也就是只认 POSIX 绝对路径。后果在 Windows 上是**静默**的：
///  用户在对话里粘一个 `D:\pics\shot.png`，这里一个候选都找不到，于是图片
///  不会被内联 —— 模型看到的是一串字面路径，然后一本正经地就着那串字回答。
///  不报错，日志里也没有任何东西。
///
///  而这条路是真的会走到的：`formats/openai.rs` 在拼消息时每条文本都过一遍，
///  也就是所有 openai 系供应商（我们发的九家里七家）都在用它。Windows 桌面端
///  是本项目的主力形态，所以这是个必须补的缺口，不是「以后再说」。
///
///  上游那 6 条 `images::tests` 在 Windows 上全红，正是因为它们用
///  `tempfile::tempdir()` 构造路径 —— **测试是对的**，POSIX-only 的是实现。
/// ══════════════════════════════════════════════════════════
///
/// 三种起点：
///
/// | 形态 | 起点 | 为什么是那里 |
/// |---|---|---|
/// | `/tmp/x.png` | `/` | POSIX 绝对路径 |
/// | `C:\pics\x.png` | **盘符** | 从 `\` 起头会把 `C:` 丢掉，而丢了盘符就不是绝对路径，`is_existing_image_path` 会直接否掉 |
/// | `\\server\share\x.png` | 那两个反斜杠 | UNC 的绝对性就在这两个字符上 |
fn candidate_anchors(window: &str) -> Vec<usize> {
    let mut at: Vec<usize> = window.match_indices('/').map(|(i, _)| i).collect();

    // `cfg!` 而不是 `#[cfg]`：两边都参与编译，POSIX 上写坏了这里同样编不过。
    // `#[cfg]` 之下 Windows 分支在 Linux CI 上根本不编译，而那正是它烂掉的方式。
    if cfg!(windows) {
        let b = window.as_bytes();
        for i in 0..b.len() {
            if !window.is_char_boundary(i) {
                continue;
            }
            // 盘符：字母 + 冒号 + 分隔符（`C:\` 或 `C:/`，两种 Windows 都认）
            let drive = i + 2 < b.len()
                && b[i].is_ascii_alphabetic()
                && b[i + 1] == b':'
                && (b[i + 2] == b'\\' || b[i + 2] == b'/');
            // UNC：开头那两个反斜杠
            let unc = i + 1 < b.len() && b[i] == b'\\' && b[i + 1] == b'\\';
            if drive || unc {
                at.push(i);
            }
        }
        at.sort_unstable();
        at.dedup();
    }
    at
}

fn clean_path(path: &str) -> Cow<'_, str> {
    if !path.contains('\\') {
        return Cow::Borrowed(path);
    }

    let mut cleaned = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();
    let mut changed = false;

    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if !next.is_alphanumeric() {
                    cleaned.push(next);
                    chars.next();
                    changed = true;
                    continue;
                }
            }
        }
        cleaned.push(c);
    }

    if changed {
        Cow::Owned(cleaned)
    } else {
        Cow::Borrowed(path)
    }
}

fn image_path_candidate(candidate: &str) -> Option<Cow<'_, str>> {
    if is_existing_image_path(candidate) {
        return Some(Cow::Borrowed(candidate));
    }

    let cleaned = clean_path(candidate);
    if cleaned.as_ref() != candidate && is_existing_image_path(cleaned.as_ref()) {
        return Some(cleaned);
    }

    None
}

fn is_existing_image_path(candidate: &str) -> bool {
    let path = Path::new(candidate);
    path.is_absolute() && path.is_file() && is_image_file(path)
}

fn is_path_leading_boundary(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '\u{00AB}' | '\u{00BB}' | '\u{2018}'
                ..='\u{201F}' | '\u{2039}' | '\u{203A}'
        )
}

fn is_path_terminator(c: char) -> bool {
    c == '/'
        || c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '\u{00AB}' | '\u{00BB}' | '\u{2013}'
                ..='\u{201F}' | '\u{2026}' | '\u{2039}' | '\u{203A}'
        )
        || ('\u{2300}'..='\u{23FF}').contains(&c)
        || ('\u{2600}'..='\u{27BF}').contains(&c)
        || ('\u{2B00}'..='\u{2BFF}').contains(&c)
        || ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
        || ('\u{1F300}'..='\u{1FAFF}').contains(&c)
}

/// Case-insensitive ASCII substring search returning a byte index into
/// `haystack` (no allocation, so the index stays valid for slicing).
fn find_ascii_ci(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let (hb, nb) = (haystack.as_bytes(), needle.as_bytes());
    if nb.is_empty() || hb.len() < nb.len() || from > hb.len() - nb.len() {
        return None;
    }
    (from..=hb.len() - nb.len()).find(|&i| {
        hb[i..i + nb.len()]
            .iter()
            .zip(nb)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}

/// Check if a file is actually an image by examining its magic bytes
fn is_image_file(path: &Path) -> bool {
    if let Ok(mut file) = std::fs::File::open(path) {
        let mut buffer = [0u8; 8]; // Large enough for most image magic numbers
        if file.read(&mut buffer).is_ok() {
            // Check magic numbers for common image formats
            return match &buffer[0..4] {
                // PNG: 89 50 4E 47
                [0x89, 0x50, 0x4E, 0x47] => true,
                // JPEG: FF D8 FF
                [0xFF, 0xD8, 0xFF, _] => true,
                // GIF: 47 49 46 38
                [0x47, 0x49, 0x46, 0x38] => true,
                _ => false,
            };
        }
    }
    false
}

/// Convert a local image file to base64 encoded ImageContent
pub fn load_image_file(path: &str) -> Result<ImageContent, ProviderError> {
    let path = Path::new(path);

    // Verify it's an image before proceeding
    if !is_image_file(path) {
        return Err(ProviderError::RequestFailed(
            "File is not a valid image".to_string(),
        ));
    }

    // Read the file
    let bytes = std::fs::read(path)
        .map_err(|e| ProviderError::RequestFailed(format!("Failed to read image file: {}", e)))?;

    // Detect mime type from extension
    let mime_type = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => match ext.to_lowercase().as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            _ => {
                return Err(ProviderError::RequestFailed(
                    "Unsupported image format".to_string(),
                ))
            }
        },
        None => {
            return Err(ProviderError::RequestFailed(
                "Unknown image format".to_string(),
            ))
        }
    };

    // Convert to base64
    let data = base64::prelude::BASE64_STANDARD.encode(&bytes);

    Ok(RawImageContent {
        mime_type: mime_type.to_string(),
        data,
        meta: None,
    }
    .no_annotation())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile;

    #[test]
    fn test_detect_image_path() {
        // Create a temporary PNG file with valid PNG magic numbers
        let temp_dir = tempfile::tempdir().unwrap();
        let png_path = temp_dir.path().join("test.png");
        let png_data = [
            0x89, 0x50, 0x4E, 0x47, // PNG magic number
            0x0D, 0x0A, 0x1A, 0x0A, // PNG header
            0x00, 0x00, 0x00, 0x0D, // Rest of fake PNG data
        ];
        std::fs::write(&png_path, png_data).unwrap();
        let png_path_str = png_path.to_str().unwrap();

        // Create a fake PNG (wrong magic numbers)
        let fake_png_path = temp_dir.path().join("fake.png");
        std::fs::write(&fake_png_path, b"not a real png").unwrap();

        // Test with valid PNG file using absolute path
        let text = format!("Here is an image {}", png_path_str);
        assert_eq!(detect_image_path(&text).as_deref(), Some(png_path_str));

        // Test with non-image file that has .png extension
        let text = format!("Here is a fake image {}", fake_png_path.to_str().unwrap());
        assert_eq!(detect_image_path(&text).as_deref(), None);

        // Test with nonexistent file
        let text = "Here is a fake.png that doesn't exist";
        assert_eq!(detect_image_path(text).as_deref(), None);

        // Test with non-image file
        let text = "Here is a file.txt";
        assert_eq!(detect_image_path(text).as_deref(), None);

        // Test with relative path (should not match)
        let text = "Here is a relative/path/image.png";
        assert_eq!(detect_image_path(text).as_deref(), None);
    }

    #[test]
    fn test_detect_image_path_with_spaces() {
        // Absolute path containing spaces (macOS screenshot style).
        let temp_dir = tempfile::tempdir().unwrap();
        let png_path = temp_dir.path().join("Screen Shot 2026.png");
        let png_data = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        std::fs::write(&png_path, png_data).unwrap();
        let png_path_str = png_path.to_str().unwrap();

        let text = format!("please describe {} for me", png_path_str);
        assert_eq!(detect_image_path(&text).as_deref(), Some(png_path_str));

        // Case-insensitive extension also matches.
        let upper = temp_dir.path().join("Another Shot.PNG");
        std::fs::write(&upper, png_data).unwrap();
        let upper_str = upper.to_str().unwrap();
        let text = format!("see {}", upper_str);
        assert_eq!(detect_image_path(&text).as_deref(), Some(upper_str));

        // Quoted path with spaces: the closing quote terminates the candidate.
        let text = format!("describe \"{}\" please", png_path_str);
        assert_eq!(detect_image_path(&text).as_deref(), Some(png_path_str));
        let text = format!("describe '{}'", png_path_str);
        assert_eq!(detect_image_path(&text).as_deref(), Some(png_path_str));
        let text = format!("describe “{}” please", png_path_str);
        assert_eq!(detect_image_path(&text).as_deref(), Some(png_path_str));
        let text = format!("describe «{}» please", png_path_str);
        assert_eq!(detect_image_path(&text).as_deref(), Some(png_path_str));

        // A stray closing quote in prose must not act as a terminator for an
        // unquoted path.
        let text = format!("here {}\" trailing", png_path_str);
        assert_eq!(detect_image_path(&text).as_deref(), Some(png_path_str));

        // When a spaced filename contains an earlier image extension, prefer
        // the longer existing candidate over the embedded prefix.
        let edited = temp_dir.path().join("Screen Shot.png edited.jpg");
        std::fs::write(&edited, png_data).unwrap();
        let edited_str = edited.to_str().unwrap();
        let prefix = temp_dir.path().join("Screen Shot.png");
        std::fs::write(&prefix, png_data).unwrap();
        let text = format!("look at {}", edited_str);
        assert_eq!(detect_image_path(&text).as_deref(), Some(edited_str));

        // With multiple distinct images, the first referenced one wins even if
        // a later one has a longer path.
        let a = temp_dir.path().join("a.png");
        std::fs::write(&a, png_data).unwrap();
        let longer = temp_dir.path().join("much-longer.png");
        std::fs::write(&longer, png_data).unwrap();
        let text = format!(
            "compare {} with {}",
            a.to_str().unwrap(),
            longer.to_str().unwrap()
        );
        assert_eq!(
            detect_image_path(&text).as_deref(),
            Some(a.to_str().unwrap())
        );
    }

    /// ⚠️ **只在 POSIX 上跑**（Cortex 加的 cfg，2026-08-19）。
    ///
    /// 这条测的是 shell 转义（`\ ` → 空格）——那是 POSIX 的概念。Windows 上
    /// `\` 是路径分隔符，把它当转义符恰恰是**错的**；而这条用例还要造一个
    /// 文件名里带字面反斜杠的文件，那在 Windows 上根本造不出来
    /// （`std::fs::write` 直接报「系统找不到指定的路径」）。
    ///
    /// 不是把失败的测试关掉：Windows 那条路由 `candidate_anchors` 覆盖，
    /// 见它的文档。
    #[cfg(unix)]
    #[test]
    fn test_detect_image_path_with_shell_escaped_metacharacters() {
        let temp_dir = tempfile::tempdir().unwrap();
        let png_data = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let png_path = temp_dir
            .path()
            .join("Bob's Project (v2) & $draft [final].png");
        std::fs::write(&png_path, png_data).unwrap();
        let png_path_str = png_path.to_str().unwrap();

        let escaped_path = png_path_str
            .replace(' ', "\\ ")
            .replace('(', "\\(")
            .replace(')', "\\)")
            .replace('&', "\\&")
            .replace('$', "\\$")
            .replace('\'', "\\'")
            .replace('[', "\\[")
            .replace(']', "\\]");
        let text = format!("please describe {}", escaped_path);

        assert_eq!(detect_image_path(&text).as_deref(), Some(png_path_str));
    }

    /// ⚠️ **只在 POSIX 上跑**（Cortex 加的 cfg，2026-08-19）。
    ///
    /// 这条测的是 shell 转义（`\ ` → 空格）——那是 POSIX 的概念。Windows 上
    /// `\` 是路径分隔符，把它当转义符恰恰是**错的**；而这条用例还要造一个
    /// 文件名里带字面反斜杠的文件，那在 Windows 上根本造不出来
    /// （`std::fs::write` 直接报「系统找不到指定的路径」）。
    ///
    /// 不是把失败的测试关掉：Windows 那条路由 `candidate_anchors` 覆盖，
    /// 见它的文档。
    #[cfg(unix)]
    #[test]
    fn test_detect_image_path_prefers_existing_literal_backslash_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let png_data = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let png_path = temp_dir.path().join("literal\\&name.png");
        std::fs::write(&png_path, png_data).unwrap();
        let png_path_str = png_path.to_str().unwrap();
        let text = format!("please describe {}", png_path_str);

        assert_eq!(detect_image_path(&text).as_deref(), Some(png_path_str));
    }

    #[test]
    fn test_detect_image_path_with_unicode_separators() {
        let temp_dir = tempfile::tempdir().unwrap();
        let png_data = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let png_path = temp_dir.path().join("photo.png");
        std::fs::write(&png_path, png_data).unwrap();
        let png_path_str = png_path.to_str().unwrap();

        assert_eq!(
            detect_image_path(&format!("{png_path_str}🙂")).as_deref(),
            Some(png_path_str)
        );
        assert_eq!(
            detect_image_path(&format!("{png_path_str}🇺🇸")).as_deref(),
            Some(png_path_str)
        );
        assert_eq!(
            detect_image_path(&format!("{png_path_str}⌚")).as_deref(),
            Some(png_path_str)
        );
        assert_eq!(
            detect_image_path(&format!("{png_path_str}⭐")).as_deref(),
            Some(png_path_str)
        );
        assert_eq!(
            detect_image_path(&format!("{png_path_str}… more text")).as_deref(),
            Some(png_path_str)
        );
        assert_eq!(
            detect_image_path(&format!("{png_path_str}\u{2014}more text")).as_deref(),
            Some(png_path_str)
        );

        assert_eq!(
            detect_image_path(&format!("{png_path_str}\u{200B}.backup")).as_deref(),
            None
        );
        assert_eq!(
            detect_image_path(&format!("{png_path_str}\u{0301}")).as_deref(),
            None
        );
        assert_eq!(
            detect_image_path(&format!("file:{png_path_str}")).as_deref(),
            None
        );
    }

    #[test]
    fn test_detect_image_path_ignores_urls_and_longer_extensions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let png_data = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

        // A real image whose path is a suffix of a URL must not be extracted
        // from that URL via the `://` separator.
        let dir = temp_dir.path().to_str().unwrap().trim_start_matches('/');
        let png_path = temp_dir.path().join("photo.png");
        std::fs::write(&png_path, png_data).unwrap();
        let url = format!("https:/{}/photo.png", dir);
        assert_eq!(detect_image_path(&url).as_deref(), None);

        // A backup file sharing the image extension prefix must not be
        // truncated to the bare image path.
        let real = temp_dir.path().join("shot.png");
        std::fs::write(&real, png_data).unwrap();
        let backup = format!("{}.backup", real.to_str().unwrap());
        assert_eq!(detect_image_path(&backup).as_deref(), None);
    }

    #[test]
    fn test_detect_image_path_ignores_extension_flood() {
        // Many extension-like tokens but no real absolute path: must scan
        // cheaply (bounded) and find nothing.
        let text = "see foo.png and bar.jpg and baz.jpeg ".repeat(500);
        assert_eq!(detect_image_path(&text).as_deref(), None);
    }

    #[test]
    fn test_load_image_file() {
        // Create a temporary PNG file with valid PNG magic numbers
        let temp_dir = tempfile::tempdir().unwrap();
        let png_path = temp_dir.path().join("test.png");
        let png_data = [
            0x89, 0x50, 0x4E, 0x47, // PNG magic number
            0x0D, 0x0A, 0x1A, 0x0A, // PNG header
            0x00, 0x00, 0x00, 0x0D, // Rest of fake PNG data
        ];
        std::fs::write(&png_path, png_data).unwrap();
        let png_path_str = png_path.to_str().unwrap();

        // Create a fake PNG (wrong magic numbers)
        let fake_png_path = temp_dir.path().join("fake.png");
        std::fs::write(&fake_png_path, b"not a real png").unwrap();
        let fake_png_path_str = fake_png_path.to_str().unwrap();

        // Test loading valid PNG file
        let result = load_image_file(png_path_str);
        assert!(result.is_ok());
        let image = result.unwrap();
        assert_eq!(image.mime_type, "image/png");

        // Test loading fake PNG file
        let result = load_image_file(fake_png_path_str);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not a valid image"));

        // Test nonexistent file
        let result = load_image_file("nonexistent.png");
        assert!(result.is_err());

        // Create a GIF file with valid header bytes
        let gif_path = temp_dir.path().join("test.gif");
        // Minimal GIF89a header
        let gif_data = [0x47, 0x49, 0x46, 0x38, 0x39, 0x61];
        std::fs::write(&gif_path, gif_data).unwrap();
        let gif_path_str = gif_path.to_str().unwrap();

        // Test loading unsupported GIF format
        let result = load_image_file(gif_path_str);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unsupported image format"));
    }
}
