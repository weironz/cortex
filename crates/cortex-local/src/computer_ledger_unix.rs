//! 非 Windows 生产路径暂不开放；保留同一协议的纯临时目录测试装配。
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
pub(super) fn global_root() -> Result<PathBuf, String> {
    Err("本候选的机器权威仅实现 Windows；禁止账户目录回退".into())
}
pub(super) fn create_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(path)
}
pub(super) fn create_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}
pub(super) fn read_locked(path: &Path) -> io::Result<File> {
    let guard = PathGuard::open(path, false, true)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    verify_open_file(&file, guard.identity())?;
    Ok(file)
}
pub(super) fn open_lock(path: &Path) -> io::Result<File> {
    let guard = PathGuard::open(path, false, true)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    verify_open_file(&file, guard.identity())?;
    Ok(file)
}
fn inspect(file: &File, directory: bool, private: bool) -> io::Result<[u8; 16]> {
    let meta = file.metadata()?;
    if (if directory {
        !meta.is_dir()
    } else {
        !meta.is_file()
    }) || (!directory && meta.nlink() != 1)
        || (private && meta.permissions().mode() & 0o077 != 0)
    {
        return Err(io::Error::other("authority 句柄边界不合格"));
    }
    let mut identity = [0; 16];
    identity[..8].copy_from_slice(&meta.dev().to_le_bytes());
    identity[8..].copy_from_slice(&meta.ino().to_le_bytes());
    Ok(identity)
}
pub(super) fn verify_open_file(file: &File, expected: [u8; 16]) -> io::Result<()> {
    if inspect(file, false, true)? != expected {
        return Err(io::Error::other("authority 锁句柄不属于原文件对象"));
    }
    Ok(())
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
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink()
            || meta.is_dir() != directory
            || (!directory && meta.nlink() != 1)
            || (private && meta.permissions().mode() & 0o077 != 0)
        {
            return Err(io::Error::other("authority 边界不合格"));
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let identity = inspect(&file, directory, private)?;
        if identity[..8] != meta.dev().to_le_bytes() || identity[8..] != meta.ino().to_le_bytes() {
            return Err(io::Error::other("authority 打开期间文件被替换"));
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
        if inspect(&self._file, self.directory, self.private)? != self.identity {
            return Err(io::Error::other("authority 原句柄身份已失效"));
        }
        if Self::open(&self.path, self.directory, self.private)?.identity != self.identity {
            return Err(io::Error::other("authority 文件被替换"));
        }
        Ok(())
    }
}
pub(super) fn hold_parents(path: &Path) -> io::Result<Vec<PathGuard>> {
    path.parent()
        .ok_or_else(|| io::Error::other("无父目录"))?
        .ancestors()
        .map(|p| PathGuard::open(p, true, false))
        .collect()
}
