//! 内容寻址：哈希计算、校验与 storage key 派生。
//!
//! # 为什么是 SHA-256 而不是更快的 BLAKE3
//!
//! 不为速度，为**契约对齐**：migration 里的 `sha256` domain 是
//! `^[0-9a-f]{64}$`，S3 生态（`x-amz-checksum-sha256`）也原生支持它。
//! 换 BLAKE3 要同时改 schema 与外部接口，省下的那点 CPU 不值这个价。

use sha2::{Digest, Sha256};

use crate::error::{BlobError, Result};

/// 十六进制哈希的字符数。与 migration 的 `sha256` domain 一致。
pub const HASH_HEX_LEN: usize = 64;

/// 对象 key 的公共前缀。留出命名空间，将来同桶放别的东西不会撞。
const KEY_PREFIX: &str = "blobs";

/// 计算内容哈希（小写十六进制）。
///
/// 这是内容寻址的**唯一**入口：同字节必得同 key，
/// 于是「上传」天然幂等、「去重」天然免费、对象天然不可变
/// —— 也正是 RustFS 不支持对象版本控制却无妨的原因：
/// 一个 key 的内容永远是同一份字节，「旧版本」这个概念根本不存在。
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// 校验哈希形如 64 位小写十六进制。
///
/// # 这是安全边界，不是格式洁癖
///
/// 哈希是唯一由外部（客户端 URL 路径、同步队列、数据库行）流入存储路径的值。
/// [`LocalFsBlobStore`](crate::LocalFsBlobStore) 要拿它拼文件路径，
/// S3 要拿它拼 object key。放进来一个 `../../../etc/passwd`，前者就是任意文件读写。
///
/// 十六进制字符集里没有 `.`、`/`、`\`、`%`、NUL，所以**这一条检查
/// 就把全部路径遍历向量堵死了**——不需要在下游再做一次转义，
/// 也不该做：两处独立的检查必然有一天不一致。
///
/// # Errors
/// 不合形制时返回 [`BlobError::InvalidHash`]。
pub fn validate_hash(hash: &str) -> Result<()> {
    let well_formed = hash.len() == HASH_HEX_LEN
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if well_formed {
        Ok(())
    } else {
        Err(BlobError::InvalidHash(hash.to_owned()))
    }
}

/// 由哈希派生对象 key：`blobs/ab/cd/<64 位哈希>`。
///
/// 两级前缀分桶不是为了好看：
/// - `LocalFsBlobStore` 若把百万个文件平铺进一个目录，ext4 的 htree
///   与 NTFS 的目录索引都会明显退化，`ls` 更是直接卡死
/// - S3 侧前缀影响分区，而哈希前缀天然均匀
///
/// 两个后端**必须共用这个函数**，key 才在后端之间可移植——
/// 从 LocalFs 迁到 RustFS 时 `blobs.storage_key` 那一列不用重写。
///
/// # Errors
/// 哈希非法时返回 [`BlobError::InvalidHash`]。
pub fn storage_key(hash: &str) -> Result<String> {
    validate_hash(hash)?;
    Ok(format!(
        "{KEY_PREFIX}/{}/{}/{hash}",
        &hash[0..2],
        &hash[2..4]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_bytes_yield_same_key() {
        let a = sha256_hex(b"cortex");
        assert_eq!(
            a,
            sha256_hex(b"cortex"),
            "内容寻址的根基：同字节必须得到同一个 key，否则去重与幂等全部失效"
        );
        assert_ne!(
            a,
            sha256_hex(b"cortey"),
            "一字之差就该是完全不同的 key，否则去重会张冠李戴"
        );
    }

    #[test]
    fn hash_matches_migration_domain() {
        let h = sha256_hex(b"");
        assert_eq!(
            h.len(),
            HASH_HEX_LEN,
            "长度须与 migration 的 sha256 domain 一致，否则入库时被 CHECK 拒绝"
        );
        assert!(
            h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "出现非小写十六进制字符，会被 Postgres 的 sha256 domain 拒绝：{h}"
        );
        // 空串的 SHA-256 是公认常量，用它兜住「算法确实是 SHA-256」这条
        assert_eq!(
            h, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "空输入的摘要与 SHA-256 标准值不符，说明算法或十六进制编码搞错了"
        );
    }

    #[test]
    fn traversal_payloads_are_rejected() {
        // 每一条都可能真实出现在 URL 路径或离线同步队列里
        let payloads = [
            "../../../etc/passwd",
            "..",
            "a/../b",
            "..%2f..%2fetc",
            // 长度恰好 64 但塞了斜杠 —— 只查长度的实现会在这里失守
            "../../../../../../../../../../../../../../../../../etc/passwd/aaa",
            "\\..\\..\\windows\\system32",
            "0000000000000000000000000000000000000000000000000000000000000/..",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b8\0\0",
        ];
        for h in payloads {
            assert!(
                validate_hash(h).is_err(),
                "路径遍历载荷未被拦截，LocalFsBlobStore 将出现任意文件读写：{h:?}"
            );
            assert!(
                storage_key(h).is_err(),
                "storage_key 未复用校验，非法哈希漏进了对象 key：{h:?}"
            );
        }
    }

    #[test]
    fn malformed_hashes_are_rejected() {
        let valid = sha256_hex(b"x");
        assert!(validate_hash(&valid).is_ok(), "合法哈希被误拒：{valid}");

        assert!(
            validate_hash(&valid.to_uppercase()).is_err(),
            "大写十六进制必须拒 —— 放行会让同一份内容产生两个 key，去重直接失效"
        );
        assert!(
            validate_hash(&valid[..HASH_HEX_LEN - 1]).is_err(),
            "截断的哈希必须拒，否则前缀碰撞会让不同内容映射到同一对象"
        );
        assert!(
            validate_hash(&format!("{valid}0")).is_err(),
            "超长的哈希必须拒"
        );
        assert!(validate_hash("").is_err(), "空哈希必须拒");
    }

    #[test]
    fn storage_key_layout_is_stable() {
        let hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(
            storage_key(hash).unwrap(),
            format!("blobs/e3/b0/{hash}"),
            "key 布局是两个后端共享的契约，改动会让已入库的 storage_key 全部指错地方"
        );
    }
}
