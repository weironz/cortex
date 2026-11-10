// SPDX-License-Identifier: Apache-2.0
mod common;

use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use common::{CurrentEpochPrepare as _, intent, permit, temporary_db};
use computer_dispatch_ledger::{Blocked, Ledger, Preparation, RecordedState};

struct TestChild {
    process: Child,
    stderr: Option<JoinHandle<io::Result<Vec<u8>>>>,
    reaped: bool,
}

impl TestChild {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        // 先取得清理所有权，再做可能失败的管道/线程装配，避免 spawn 成功后漏掉孩子。
        let mut child = Self {
            process: command.spawn()?,
            stderr: None,
            reaped: false,
        };
        let mut stderr = child
            .process
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("测试子进程必须提供 stderr 管道"))?;
        // 持续排空诊断，不能让管道写满把正常退出变成父测试超时。
        child.stderr = Some(thread::Builder::new().spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes)?;
            Ok(bytes)
        })?);
        Ok(child)
    }

    fn stop_and_reap(&mut self) -> io::Result<()> {
        // 只操作本对象 spawn 返回的句柄；kill 与自然退出竞争时，仍以 wait 回收为准。
        let kill_error = self.process.kill().err();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.process.try_wait() {
                Ok(Some(_)) => {
                    self.reaped = true;
                    return Ok(());
                }
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("终止后五秒内仍未回收；kill 错误：{kill_error:?}"),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn collect(&mut self, deadline: Instant) -> Result<Output, String> {
        let status = loop {
            match self.process.try_wait() {
                Ok(Some(status)) => {
                    self.reaped = true;
                    break Ok(status);
                }
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => break Err("等待子进程超过 30 秒".to_owned()),
                Err(error) => break Err(format!("等待子进程失败：{error}")),
            }
        };
        if !self.reaped {
            self.stop_and_reap()
                .map_err(|error| format!("{status:?}；终止并回收也失败：{error}"))?;
        }
        let stderr = self
            .stderr
            .take()
            .ok_or_else(|| "缺少子进程诊断读取线程".to_owned())?
            .join()
            .map_err(|_| "子进程诊断读取线程 panic".to_owned())?
            .map_err(|error| format!("读取子进程诊断失败：{error}"))?;
        let status = status
            .map_err(|error| format!("{error}；stderr：{}", String::from_utf8_lossy(&stderr)))?;
        Ok(Output {
            status,
            stdout: Vec::new(),
            stderr,
        })
    }
}

impl Drop for TestChild {
    fn drop(&mut self) {
        // 即使父测试在 spawn/wait 装配期间展开栈，也不能把孩子留给别的测试清理。
        if !self.reaped {
            let _ = self.stop_and_reap();
        }
        if self.reaped
            && let Some(reader) = self.stderr.take()
        {
            let _ = reader.join();
        }
    }
}

fn collect_children(commands: impl IntoIterator<Item = Command>) -> Vec<Output> {
    let mut children = Vec::new();
    let mut errors = Vec::new();
    // 必须先启动全部竞争者；回收不得把四进程争抢退化成顺序执行。
    for (index, mut command) in commands.into_iter().enumerate() {
        match TestChild::spawn(&mut command) {
            Ok(child) => children.push((index, child)),
            Err(error) => errors.push(format!("子进程 {index} 启动失败：{error}")),
        }
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut outputs = Vec::new();
    for (index, child) in &mut children {
        match child.collect(deadline) {
            Ok(output) => outputs.push(output),
            Err(error) => errors.push(format!(
                "子进程 {index}（PID {}）：{error}",
                child.process.id()
            )),
        }
    }
    drop(children);
    // 所有孩子都已收集并回收后才判定错误；不重试 SQLite 1546，也不改写其退出码。
    assert!(errors.is_empty(), "父 harness 失败：{}", errors.join("\n"));
    outputs
}

fn child_command(path: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("应能找到当前测试可执行文件"));
    command
        .args([
            "--exact",
            "prepare_then_exit_child",
            "--ignored",
            "--nocapture",
        ])
        .env("COMPUTER_LEDGER_TEST_CHILD_DB", path)
        .env_remove("COMPUTER_LEDGER_TEST_SNAPSHOT_SHM")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

#[test]
#[ignore = "仅供父测试以专用环境变量启动的退出测试进程"]
fn prepare_then_exit_child() {
    let path = std::env::var_os("COMPUTER_LEDGER_TEST_CHILD_DB").expect("父测试必须提供测试库路径");
    let path = std::path::PathBuf::from(path);
    let ledger = Ledger::open(&path).expect("子进程应打开已有账本");
    #[cfg(windows)]
    if std::env::var_os("COMPUTER_LEDGER_TEST_SNAPSHOT_SHM").is_some() {
        windows_mapped_shm::save_before_prepare(&path);
    }
    match ledger
        .prepare_current(intent(1, 2, 3))
        .expect("子进程应完成 prepare")
    {
        Preparation::Permit(granted) => {
            // process::exit 不运行 Permit/Ledger 的 Drop；不是物理断电模拟。
            let _permit = permit(Preparation::Permit(granted));
            std::process::exit(73);
        }
        Preparation::Blocked(_) => std::process::exit(74),
        Preparation::Existing(_) => std::process::exit(75),
    }
}

#[test]
fn process_exit_after_prepare_leaves_persistent_fence() {
    let (_directory, path) = temporary_db();
    drop(Ledger::create(&path).expect("父进程应初始化账本"));
    let outputs = collect_children([child_command(&path)]);
    let output = &outputs[0];
    assert_eq!(
        output.status.code(),
        Some(73),
        "子进程应在取得 Permit 后直接 exit：{output:?}"
    );
    assert_persistent_fence(&path);
}

fn assert_persistent_fence(path: &Path) {
    let reopened = Ledger::open(path).expect("子进程退出后应能恢复读取 SQLite WAL");
    assert!(
        reopened.has_fences().expect("恢复后应能查询已提交的 fence"),
        "恢复 WAL 后必须保留子进程提交的 fence"
    );
    let old = reopened
        .prepare_current(intent(1, 2, 3))
        .expect("重开应给出明确决策");
    assert!(
        matches!(
            old,
            Preparation::Blocked(Blocked::OperationFenced(RecordedState::Prepared))
        ),
        "prepare 后直接退出不得允许重派：{old:?}"
    );
    let cross_subject = reopened
        .prepare_current(intent(4, 5, 3))
        .expect("新主体应看见持久 fence");
    assert!(
        matches!(cross_subject, Preparation::Blocked(Blocked::ResourceBusy)),
        "子进程留下的 fence 必须跨主体：{cross_subject:?}"
    );
}

#[test]
fn concurrent_processes_get_only_one_permit() {
    let (_directory, path) = temporary_db();
    drop(Ledger::create(&path).expect("父进程应初始化账本"));
    let outputs = collect_children((0..4).map(|_| child_command(&path)));
    let granted = outputs
        .iter()
        .filter(|output| output.status.code() == Some(73))
        .count();
    let blocked = outputs
        .iter()
        .filter(|output| output.status.code() == Some(74))
        .count();
    assert_eq!(outputs.len(), 4, "必须收集四个竞争子进程：{outputs:?}");
    assert_eq!(granted, 1, "四个进程只能恰有一个 exit 73：{outputs:?}");
    assert_eq!(blocked, 3, "其余三个进程必须全部 exit 74：{outputs:?}");
}

#[cfg(windows)]
#[test]
fn windows_current_shm_mapping_preserves_prepared_fence() {
    windows_mapped_shm::recover_with_mapping(false);
}

#[cfg(windows)]
#[test]
fn windows_stale_shm_mapping_preserves_prepared_fence() {
    windows_mapped_shm::recover_with_mapping(true);
}

#[cfg(windows)]
mod windows_mapped_shm {
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};

    use windows_sys::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
        PAGE_READONLY, UnmapViewOfFile,
    };

    use super::{Ledger, assert_persistent_fence, child_command, collect_children, temporary_db};

    fn shm_path(path: &Path) -> PathBuf {
        let mut name = path.as_os_str().to_os_string();
        name.push("-shm");
        PathBuf::from(name)
    }

    pub(super) fn save_before_prepare(path: &Path) {
        std::fs::copy(shm_path(path), path.with_extension("before-prepare-shm"))
            .expect("子进程应在 prepare 提交之前保存旧 SHM");
    }

    struct ShmMapping {
        view: MEMORY_MAPPED_VIEW_ADDRESS,
        _mapping: OwnedHandle,
        _file: File,
    }

    impl ShmMapping {
        fn open(path: &Path) -> io::Result<Self> {
            // 调用点只传本测试 TempDir 内的实际 -shm，绝不映射用户账本或旁边的快照。
            let file = File::open(path)?;
            // SAFETY：文件句柄存活；无名称、无继承属性，零长度表示现有文件的完整只读映射。
            let raw_mapping = unsafe {
                CreateFileMappingW(
                    file.as_raw_handle(),
                    std::ptr::null(),
                    PAGE_READONLY,
                    0,
                    0,
                    std::ptr::null(),
                )
            };
            if raw_mapping.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY：CreateFileMappingW 成功返回独占拥有的句柄，由 OwnedHandle 唯一关闭。
            let mapping = unsafe { OwnedHandle::from_raw_handle(raw_mapping) };
            // SAFETY：映射句柄有效；只读访问整个现有文件，不解引用映射内可被 SQLite 更新的数据。
            let view = unsafe { MapViewOfFile(mapping.as_raw_handle(), FILE_MAP_READ, 0, 0, 0) };
            if view.Value.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                view,
                _mapping: mapping,
                _file: file,
            })
        }
    }

    impl Drop for ShmMapping {
        fn drop(&mut self) {
            // SAFETY：view 由 MapViewOfFile 成功取得且只在此释放；随后才关闭映射/文件句柄。
            unsafe {
                UnmapViewOfFile(self.view);
            }
        }
    }

    pub(super) fn recover_with_mapping(stale: bool) {
        let (_directory, path) = temporary_db();
        drop(Ledger::create(&path).expect("父进程应初始化私有账本"));
        let mut command = child_command(&path);
        command.env("COMPUTER_LEDGER_TEST_SNAPSHOT_SHM", "1");
        let outputs = collect_children([command]);
        assert_eq!(
            outputs[0].status.code(),
            Some(73),
            "映射前子进程必须已经持久提交 Prepared 并直接 exit：{outputs:?}"
        );

        let shm = shm_path(&path);
        let before = std::fs::read(path.with_extension("before-prepare-shm"))
            .expect("应读取 prepare 之前的 SHM 快照");
        let current = std::fs::read(&shm).expect("直接退出后实际 SHM 必须仍然存在");
        assert!(!before.is_empty(), "旧 SHM 快照不能是空文件");
        assert_eq!(before.len(), current.len(), "恢复旧索引不得改变 SHM 长度");
        assert_ne!(
            before, current,
            "提交 Prepared 必须确实更新 SHM，旧快照才能覆盖恢复回归"
        );
        if stale {
            // 只恢复派生索引，不回滚 WAL/数据库；保持同一路径和文件身份以迫使 WAL 重建 fence。
            let mut file = OpenOptions::new()
                .write(true)
                .open(&shm)
                .expect("应打开本测试已有 SHM，不创建或替换文件");
            file.write_all(&before)
                .expect("应原位恢复 prepare 之前的 SHM");
            file.sync_all().expect("恢复旧 SHM 后应同步文件");
            assert_eq!(
                std::fs::read(&shm).expect("应复核旧 SHM"),
                before,
                "映射前必须确实恢复旧索引"
            );
        }

        // 对应 SQLite 7c103a0b7d：真实映射仍存活时，Windows 不允许把 SHM 截成零长。
        // 只开 File 不足以重现；必须同时持有 CreateFileMappingW 和 MapViewOfFile 的结果。
        let mapping = ShmMapping::open(&shm).expect("应映射本测试实际 SHM");
        assert_persistent_fence(&path);
        drop(mapping);
    }
}
