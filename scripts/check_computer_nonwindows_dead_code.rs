//! 非 Windows 目标上的轻量装配：只调用生产中的租约构造和历史读取。
#![deny(dead_code)]
extern crate self as sha2;

pub trait Digest {
    fn digest<T: AsRef<[u8]>>(input: T) -> DummyHash;
}
pub struct Sha256;
pub struct DummyHash;
impl Digest for Sha256 {
    fn digest<T: AsRef<[u8]>>(_input: T) -> DummyHash {
        DummyHash
    }
}
impl std::fmt::LowerHex for DummyHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fixture")
    }
}

#[path = "../crates/cortex-local/src/computer_lease.rs"]
mod computer_lease;
#[path = "../crates/cortex-local/src/computer_trace_gap.rs"]
mod computer_trace_gap;

/// 模拟非桌面宿主构造 registry 和调用历史端点，不人为调用写侧。
pub fn assemble_and_read(root: &std::path::Path, session: &str) -> bool {
    let _leases = computer_lease::ComputerLeaseRegistry::new();
    match computer_trace_gap::check::<(), computer_trace_gap::GapError>(root, session, || Ok(())) {
        Ok(()) => true,
        Err(computer_trace_gap::GapError::Incomplete) => false,
        Err(computer_trace_gap::GapError::Io(error)) => {
            error.kind() == std::io::ErrorKind::NotFound
        }
    }
}
