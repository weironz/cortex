// Windows 句柄/ACL 协调租约改编自 Cua（MIT）。
// Copyright (c) 2025 Cua AI, Inc.
// Source: cua-driver-core/src/perception_client/containment/windows.rs::AclLease,
// history.rs::WriterLease, inspected 85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19.
// 完整许可：crates/computer-dispatch-ledger/LICENSE-MIT-CUA。
// 改动：永久私有权限（不恢复宽 ACL）、固定机器根、祖先 no-reparse 句柄、
// 文件身份与硬链接拒绝。fs2 独占锁改用 std::fs::File::try_lock。

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

#[path = "computer_ledger_install_windows.rs"]
pub(crate) mod installation;

use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    GetSecurityDescriptorControl, GetTokenInformation, OWNER_SECURITY_INFORMATION,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
};
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::{CSIDL_COMMON_APPDATA, SHGFP_TYPE_CURRENT, SHGetFolderPathW};

fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain([0]).collect()
}
fn invalid(message: &str) -> io::Error {
    io::Error::other(message)
}
struct Local(*mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
struct Sid {
    _buffer: Vec<usize>,
    pointer: *mut c_void,
}
fn current_sid() -> io::Result<Sid> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut size = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut size);
    }
    let mut buffer = vec![0_usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    };
    unsafe {
        CloseHandle(token);
    }
    if ok == 0 || size < std::mem::size_of::<TOKEN_USER>() as u32 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Vec<usize> 保证 TOKEN_USER 对齐，Win32 已成功填充且长度足够。
    let pointer = unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    Ok(Sid {
        _buffer: buffer,
        pointer,
    })
}
fn sid_text(pointer: *mut c_void) -> io::Result<String> {
    let mut output = null_mut();
    if unsafe { ConvertSidToStringSidW(pointer, &mut output) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _free = Local(output.cast());
    let mut length = 0;
    while unsafe { *output.add(length) } != 0 {
        length += 1;
    }
    Ok(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(output, length)
    }))
}
fn descriptor() -> io::Result<Local> {
    let user = current_sid()?;
    let sid = sid_text(user.pointer)?;
    let text = wide(format!(
        "O:{sid}D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
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
        return Err(io::Error::last_os_error());
    }
    Ok(Local(descriptor))
}
fn verify_acl(file: &File, require_protected: bool) -> io::Result<()> {
    let mut owner = null_mut();
    let mut acl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 || owner.is_null() || acl.is_null() {
        return Err(invalid("authority 缺少可验证 owner/DACL"));
    }
    let _free = Local(descriptor);
    let user = current_sid()?;
    if unsafe { EqualSid(owner, user.pointer) } == 0 {
        return Err(invalid("authority 不属于当前受信 OS owner，不可接管"));
    }
    let mut control = 0;
    let mut revision = 0;
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if require_protected && control & 0x1000 == 0 {
        return Err(invalid("authority root DACL 未保护"));
    }
    // SAFETY: GetSecurityInfo 提供有效 ACL，GetAce 对每个索引再次核验。
    let count = unsafe { (*acl).AceCount };
    if count == 0 || unsafe { (*acl).AclRevision } < ACL_REVISION as u8 {
        return Err(invalid("authority ACL 为空/无效"));
    }
    let owner_sid = sid_text(user.pointer)?;
    let allowed = [owner_sid.as_str(), "S-1-5-18", "S-1-5-32-544"];
    for index in 0..u32::from(count) {
        let mut ace = null_mut();
        if unsafe { GetAce(acl, index, &mut ace) } == 0 || ace.is_null() {
            return Err(io::Error::last_os_error());
        }
        let ace = unsafe { &*(ace.cast::<ACCESS_ALLOWED_ACE>()) };
        if u32::from(ace.Header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
            return Err(invalid("authority 包含不支持的 ACL 项"));
        }
        let sid = sid_text(std::ptr::addr_of!(ace.SidStart).cast_mut().cast())?;
        if !allowed.contains(&sid.as_str()) {
            return Err(invalid("authority ACL 向其他主体开放"));
        }
    }
    Ok(())
}

pub(super) fn global_root() -> Result<PathBuf, String> {
    let mut buffer = [0_u16; 260];
    let result = unsafe {
        SHGetFolderPathW(
            null_mut(),
            CSIDL_COMMON_APPDATA as i32,
            null_mut(),
            SHGFP_TYPE_CURRENT as u32,
            buffer.as_mut_ptr(),
        )
    };
    if result < 0 {
        return Err("无法从 OS KnownFolder 读取机器级权威根；不接受环境变量回退".into());
    }
    let length = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    Ok(
        PathBuf::from(String::from_utf16_lossy(&buffer[..length]))
            .join("CortexComputerDispatch-v1"),
    )
}

pub(super) fn create_directory(path: &Path) -> io::Result<()> {
    let descriptor = descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    if unsafe { CreateDirectoryW(wide(path).as_ptr(), &attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
pub(super) fn create_file(path: &Path) -> io::Result<File> {
    // 文件由私有 root 继承；外置 anchor 显式应用同一私有描述符，不能继承
    // ProgramData 的通用写权限。用 Windows OpenOptions 扩展原子创建。
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    };
    let descriptor = descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            FILE_SHARE_READ,
            &attributes,
            CREATE_NEW,
            FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle.cast()) })
}
pub(super) fn read_locked(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    inspect(&file, false)?;
    verify_acl(&file, false)?;
    Ok(file)
}

/// 维护期间排斥原 Actor/LivePermit 的真实数据读句柄；不能以 metadata guard 替代。
pub(super) fn read_exclusive_key(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    private_file_identity(&file)?;
    Ok(file)
}

/// 原址改写既有锚，不 truncate、不创建、不允许删除替换。
pub(super) fn open_upgrade_anchor(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    private_file_identity(&file)?;
    Ok(file)
}

/// 只检查调用方已持有的同一句柄；独占 key 存活时不能再按路径打开第二份 key。
pub(super) fn private_file_identity(file: &File) -> io::Result<[u8; 16]> {
    let identity = inspect(file, false)?;
    verify_acl(file, false)?;
    Ok(identity)
}
pub(super) fn open_lock(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    inspect(&file, false)?;
    verify_acl(&file, false)?;
    Ok(file)
}

pub(super) fn verify_open_file(file: &File, expected: [u8; 16]) -> io::Result<()> {
    // 锁的是这个实际句柄，不能用另一次按路径打开的结果替它证明身份。
    if inspect(file, false)? != expected {
        return Err(invalid("authority 锁句柄不属于原文件对象"));
    }
    verify_acl(file, false)
}

fn inspect(file: &File, directory: bool) -> io::Result<[u8; 16]> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & 0x400 != 0 || (info.dwFileAttributes & 0x10 != 0) != directory {
        return Err(invalid("authority 拒绝 reparse/类型不符"));
    }
    if !directory && info.nNumberOfLinks != 1 {
        return Err(invalid("authority 拒绝硬链接别名"));
    }
    let mut identity = [0; 16];
    for (index, value) in [
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
        u32::from(directory),
    ]
    .into_iter()
    .enumerate()
    {
        identity[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    Ok(identity)
}

pub(super) struct PathGuard {
    path: PathBuf,
    _file: File,
    identity: [u8; 16],
    directory: bool,
    private: bool,
}
impl PathGuard {
    pub(super) fn open(path: &Path, directory: bool, private: bool) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .access_mode(0x80 | 0x0002_0000)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        let identity = inspect(&file, directory)?;
        if private {
            verify_acl(&file, directory)?;
        }
        Ok(Self {
            path: path.to_owned(),
            _file: file,
            identity,
            directory,
            private,
        })
    }
    pub(super) fn identity(&self) -> [u8; 16] {
        self.identity
    }
    pub(super) fn try_clone(&self) -> io::Result<Self> {
        self.verify()?;
        Ok(Self {
            path: self.path.clone(),
            _file: self._file.try_clone()?,
            identity: self.identity,
            directory: self.directory,
            private: self.private,
        })
    }
    pub(super) fn verify(&self) -> io::Result<()> {
        // 路径仍指向原对象还不够；新增硬链接或原句柄权限变化也必须拒绝。
        if inspect(&self._file, self.directory)? != self.identity {
            return Err(invalid("authority 原句柄身份已失效"));
        }
        if self.private {
            verify_acl(&self._file, self.directory)?;
        }
        let current = Self::open(&self.path, self.directory, self.private)?;
        if current.identity != self.identity {
            return Err(invalid("authority 文件对象被替换"));
        }
        Ok(())
    }
}
pub(super) fn hold_parents(path: &Path) -> io::Result<Vec<PathGuard>> {
    let mut ancestors: Vec<_> = path
        .parent()
        .ok_or_else(|| invalid("缺少父目录"))?
        .ancestors()
        .collect();
    ancestors.reverse();
    ancestors
        .into_iter()
        .map(|path| PathGuard::open(path, true, false))
        .collect()
}
