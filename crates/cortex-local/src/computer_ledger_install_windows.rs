//! 机器级一次性安装记录。只允许显式维护命令写 HKLM；health/派发只读。
//! 不删除记录：先持久 claim，再创建文件；中断后必须恢复，不能重新铸空历史。
use super::*;
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::Security::{TOKEN_ELEVATION, TokenElevation};
use windows_sys::Win32::System::Registry::*;

#[path = "computer_ledger_install_codec.rs"]
mod codec;
pub(crate) use codec::{
    InstallationRecord, InstallationState, MAX_INSTALLATION_BYTES, UpgradeJournal, UpgradeStage,
    UpgradeTarget, decode_state, encode_ready, encode_upgrade, validate_upgrade_cas,
};

const KEY: &str = "SOFTWARE\\CortexComputerDispatchAuthority-v1";
const VALUE: &str = "Installation";
enum Origin {
    Os,
    #[cfg(test)]
    Fixture,
}
pub(crate) struct TrustedProvision {
    owner: String,
    origin: Origin,
}
impl TrustedProvision {
    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }
    #[cfg(test)]
    pub(crate) fn fixture() -> Result<Self, String> {
        Ok(Self {
            owner: current_owner()?,
            origin: Origin::Fixture,
        })
    }
    pub(crate) fn require_global(&self) -> Result<(), String> {
        match self.origin {
            Origin::Os => Ok(()),
            #[cfg(test)]
            Origin::Fixture => Err("fixture proof 不能用于全局 provision".into()),
        }
    }
}
pub(crate) fn current_owner() -> Result<String, String> {
    sid_text(current_sid().map_err(|_| "无法读取 OS owner")?.pointer)
        .map_err(|_| "无法读取 OS owner".into())
}
fn checked_owner(
    expected: &str,
    actual: String,
    elevated: bool,
    admin: bool,
) -> Result<TrustedProvision, String> {
    if expected != actual {
        return Err("owner_mismatch：必须显式指定当前进程及将来宿主的同一 OS SID".into());
    }
    if !elevated || !admin {
        return Err(
            "administrator_required：请由同一 OS owner 的受信管理员安装流程运行；命令不会自动提权"
                .into(),
        );
    }
    Ok(TrustedProvision {
        owner: actual,
        origin: Origin::Os,
    })
}
pub(crate) fn authorize(expected: &str) -> Result<TrustedProvision, String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err("无法检查安装令牌".into());
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut size = 0;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            std::ptr::from_mut(&mut elevation).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
    };
    unsafe {
        CloseHandle(token);
    }
    if ok == 0 {
        return Err("无法检查安装令牌".into());
    }
    checked_owner(
        expected,
        current_owner()?,
        elevation.TokenIsElevated != 0,
        unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() } != 0,
    )
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
fn opened(access: u32) -> Result<Option<Key>, String> {
    let mut key = null_mut();
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            wide(KEY).as_ptr(),
            REG_OPTION_OPEN_LINK,
            access | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if result != ERROR_SUCCESS {
        return Err("无法读取机器安装记录；拒绝回退".into());
    }
    let key = Key(key);
    let mut len = 0;
    let link = unsafe {
        RegQueryValueExW(
            key.0,
            wide("SymbolicLinkValue").as_ptr(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut len,
        )
    };
    if link != ERROR_FILE_NOT_FOUND {
        return Err("机器安装记录为链接或不可验证".into());
    }
    Ok(Some(key))
}
fn encode(owner: &str, fingerprint: Option<[u8; 32]>) -> Result<Vec<u8>, String> {
    encode_ready(owner, fingerprint)
}
pub(crate) fn decode(bytes: &[u8]) -> Result<(String, Option<[u8; 32]>), String> {
    codec::decode_ready(bytes)
}
fn write(key: &Key, owner: &str, hash: Option<[u8; 32]>) -> Result<(), String> {
    let data = encode(owner, hash)?;
    write_raw(key, &data)
}
fn write_raw(key: &Key, data: &[u8]) -> Result<(), String> {
    if data.len() > MAX_INSTALLATION_BYTES {
        return Err("provision_incomplete：安装值超过 8192 字节上界".into());
    }
    if unsafe {
        RegSetValueExW(
            key.0,
            wide(VALUE).as_ptr(),
            0,
            REG_BINARY,
            data.as_ptr(),
            data.len() as u32,
        )
    } != ERROR_SUCCESS
        || unsafe { RegFlushKey(key.0) } != ERROR_SUCCESS
    {
        return Err("provision_incomplete：安装记录未确认持久，禁止重建".into());
    }
    Ok(())
}
pub(crate) fn read() -> Result<Option<InstallationRecord>, String> {
    let Some(key) = opened(KEY_QUERY_VALUE)? else {
        return Ok(None);
    };
    decode(&read_raw(&key)?).map(Some)
}

fn read_raw(key: &Key) -> Result<Vec<u8>, String> {
    let mut data = [0_u8; MAX_INSTALLATION_BYTES];
    let mut len = data.len() as u32;
    let mut kind = 0;
    let result = unsafe {
        RegQueryValueExW(
            key.0,
            wide(VALUE).as_ptr(),
            null_mut(),
            &mut kind,
            data.as_mut_ptr(),
            &mut len,
        )
    };
    if result != ERROR_SUCCESS || kind != REG_BINARY || len as usize > data.len() {
        return Err("provision_incomplete：已有安装 claim 无完整内容".into());
    }
    Ok(data[..len as usize].to_vec())
}

/// 升级维护只读接口；Fixture 仅使用纯 codec，不能借维护读操作触碰 HKLM。
pub(crate) fn read_state(
    proof: &TrustedProvision,
) -> Result<Option<(Vec<u8>, InstallationState)>, String> {
    proof.require_global()?;
    let Some(key) = opened(KEY_QUERY_VALUE)? else {
        return Ok(None);
    };
    let bytes = read_raw(&key)?;
    let state = decode_state(&bytes)?;
    let owner = match &state {
        InstallationState::Ordinary((owner, _)) => owner,
        InstallationState::Upgrade(journal) => &journal.owner,
    };
    if owner != proof.owner() {
        return Err("owner_mismatch：升级记录不属于原受信 OS owner".into());
    }
    Ok(Some((bytes, state)))
}

/// 注册表不提供这里所需的跨进程 CAS：调用方必须持续持有原 key 独占及
/// ACL/writer 维护锁。这里只对同一个已打开的 Installation 值比较完整 bytes。
/// stage/摘要不证明 anchor MAC、Core 迁移或执行者静止；那些由可信协调器验证。
pub(crate) fn compare_exchange_upgrade(
    proof: &TrustedProvision,
    expected: &[u8],
    next: &[u8],
) -> Result<(), String> {
    proof.require_global()?;
    let key = opened(KEY_QUERY_VALUE | KEY_SET_VALUE)?.ok_or("升级 CAS 的原安装 claim 丢失")?;
    let current = read_raw(&key)?;
    validate_upgrade_cas(proof.owner(), &current, expected, next)?;
    // Set 成功但 Flush 失败时也返回 Err；调用方保留 blocked 状态并重新核对实际值。
    write_raw(&key, next)
}
pub(crate) fn begin(proof: &TrustedProvision) -> Result<(), String> {
    proof.require_global()?;
    // 注册表 owner 为 Administrators；普通宿主只有读取权，不能借同 SID 删除 claim。
    let text = wide(format!(
        "O:BAD:P(A;;KA;;;SY)(A;;KA;;;BA)(A;;KR;;;{})",
        proof.owner()
    ));
    let mut descriptor = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err("无法建立安装记录 ACL".into());
    }
    let _descriptor = Local(descriptor);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let mut key = null_mut();
    let mut disposition = 0;
    let result = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            wide(KEY).as_ptr(),
            0,
            null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY,
            &attributes,
            &mut key,
            &mut disposition,
        )
    };
    if result != ERROR_SUCCESS {
        return Err("无法排他创建机器安装 claim".into());
    }
    let key = Key(key);
    if disposition != REG_CREATED_NEW_KEY {
        return Err("安装 claim 已存在；不覆盖、不重建历史".into());
    }
    write(&key, proof.owner(), None)
}
pub(crate) fn complete(proof: &TrustedProvision, hash: [u8; 32]) -> Result<(), String> {
    proof.require_global()?;
    let key = opened(KEY_QUERY_VALUE | KEY_SET_VALUE)?.ok_or("安装 claim 丢失")?;
    if decode(&read_raw(&key)?)? != (proof.owner().to_owned(), None) {
        return Err("安装 claim 已改变；拒绝覆盖".into());
    }
    write(&key, proof.owner(), Some(hash))
}
#[cfg(test)]
pub(crate) fn fixture_bytes(
    proof: &TrustedProvision,
    hash: Option<[u8; 32]>,
) -> Result<Vec<u8>, String> {
    encode(proof.owner(), hash)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_owner_and_elevated_administrator_are_both_required() {
        for (expected, elevated, admin) in [
            ("other", true, true),
            ("same", false, true),
            ("same", true, false),
        ] {
            assert!(
                checked_owner(expected, "same".into(), elevated, admin).is_err(),
                "SID 或任一 OS 权限不符必须拒绝"
            );
        }
        assert!(
            checked_owner("same", "same".into(), true, true).is_ok(),
            "受信 OS owner 的管理员安装可得到类型凭证"
        );
    }
}

#[cfg(test)]
#[path = "computer_ledger_install_codec_test.rs"]
mod codec_tests;
