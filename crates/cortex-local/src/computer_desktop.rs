//! Windows 10/11 的交互桌面证据。前台窗口可以短暂为空，不能拿它代替锁屏状态。
//! SessionFlags 依据微软 WTSINFOEX_LEVEL1_W；安全输入桌面仍必须 fail closed。
use std::ffi::c_void;
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::RemoteDesktop::{
    WTS_SESSIONSTATE_UNLOCK, WTSActive, WTSFreeMemory, WTSINFOEXW, WTSQuerySessionInformationW,
    WTSSessionInfoEx,
};
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetProcessWindowStation, GetUserObjectInformationW,
    OpenInputDesktop, UOI_NAME,
};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

fn evidence_allowed(state: i32, flags: u32, station: &str, desktop: &str) -> bool {
    state == WTSActive
        && flags == WTS_SESSIONSTATE_UNLOCK
        && station.eq_ignore_ascii_case("WinSta0")
        && desktop.eq_ignore_ascii_case("Default")
}

fn object_name(handle: *mut c_void) -> Result<String, String> {
    let mut needed = 0;
    unsafe {
        GetUserObjectInformationW(handle, UOI_NAME, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > 1024 {
        return Err("无法核对输入桌面名称；已按安全桌面拒绝".into());
    }
    let mut name = vec![0u16; (needed as usize).div_ceil(2)];
    if unsafe {
        GetUserObjectInformationW(
            handle,
            UOI_NAME,
            name.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err("无法读取输入桌面身份；已按安全桌面拒绝".into());
    }
    let end = name
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(name.len());
    String::from_utf16(&name[..end]).map_err(|_| "输入桌面身份不是合法UTF-16".into())
}

pub(crate) fn ensure() -> Result<(), String> {
    let mut session = 0;
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } == 0 || session == 0 {
        return Err("当前进程不在可确认的交互会话，拒绝电脑操作".into());
    }
    let mut buffer = std::ptr::null_mut();
    let mut bytes = 0;
    if unsafe {
        WTSQuerySessionInformationW(
            std::ptr::null_mut(),
            session,
            WTSSessionInfoEx,
            &mut buffer,
            &mut bytes,
        )
    } == 0
    {
        return Err("无法核对Windows会话锁屏状态；拒绝电脑操作".into());
    }
    struct WtsBuffer(*mut u16);
    impl Drop for WtsBuffer {
        fn drop(&mut self) {
            unsafe {
                WTSFreeMemory(self.0.cast());
            }
        }
    }
    let _buffer = WtsBuffer(buffer);
    if buffer.is_null() || (bytes as usize) < std::mem::size_of::<WTSINFOEXW>() {
        return Err("Windows会话信息不完整；已按锁屏状态拒绝".into());
    }
    let info = unsafe { &*buffer.cast::<WTSINFOEXW>() };
    if info.Level != 1 {
        return Err("Windows会话状态版本未知；已按锁屏拒绝".into());
    }
    let detail = unsafe { &info.Data.WTSInfoExLevel1 };
    if detail.SessionId != session {
        return Err("Windows返回了其他交互会话；拒绝操作".into());
    }
    let station = object_name(unsafe { GetProcessWindowStation() })?;
    let desktop = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    if desktop.is_null() {
        return Err("无法访问当前输入桌面，可能处于锁屏/UAC安全桌面；拒绝操作".into());
    }
    struct Desktop(windows_sys::Win32::System::StationsAndDesktops::HDESK);
    impl Drop for Desktop {
        fn drop(&mut self) {
            unsafe {
                CloseDesktop(self.0);
            }
        }
    }
    let _desktop = Desktop(desktop);
    let name = object_name(desktop)?;
    if !evidence_allowed(
        detail.SessionState,
        detail.SessionFlags as u32,
        &station,
        &name,
    ) {
        return Err(
            "当前Windows会话不是已解锁的活动默认输入桌面；锁屏/断连/安全桌面需交给用户".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unlocked_default_desktop_does_not_require_a_foreground_window() {
        assert!(evidence_allowed(WTSActive, 1, "WinSta0", "Default"));
        assert!(evidence_allowed(WTSActive, 1, "winsta0", "default"));
    }
    #[test]
    fn lock_disconnect_unknown_and_secure_desktop_never_pass() {
        for (state, flags, station, desktop) in [
            (WTSActive, 0, "WinSta0", "Default"),
            (WTSActive, u32::MAX, "WinSta0", "Default"),
            (4, 1, "WinSta0", "Default"),
            (WTSActive, 1, "Service-0x0", "Default"),
            (WTSActive, 1, "WinSta0", "Winlogon"),
        ] {
            assert!(
                !evidence_allowed(state, flags, station, desktop),
                "不能放宽锁屏/断开/安全桌面：{state}/{flags}/{station}/{desktop}"
            );
        }
    }
}
