//! 旧 trace 无法表达结果时的保守不完整标记。
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as _;
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use std::{fs::OpenOptions, io::Write as _, sync::mpsc};

#[cfg(any(target_os = "windows", target_os = "macos"))]
const QUEUE_CAPACITY: usize = 64;
#[cfg(any(target_os = "windows", target_os = "macos"))]
const MAX_AUDIT_BYTES: usize = 1024 * 1024;
const MAX_TRACKED_SESSIONS: usize = 4096;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

type Key = (PathBuf, String);

#[derive(Default)]
struct State {
    memory_gaps: HashSet<Key>,
    pending: HashMap<Key, usize>,
    active_scans: HashMap<Key, ScanState>,
    /// 只污染发生写入故障的主体根，不能让一个账号把其他账号的历史
    /// 一起变成 409。
    unknown_roots: HashSet<PathBuf>,
}

#[derive(Default)]
struct ScanState {
    readers: usize,
    revision: u64,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

fn lock() -> MutexGuard<'static, State> {
    state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn bump(state: &mut State, key: &Key) {
    // 只有在途扫描需要版本；无人扫描的成功写入无需留下永久条目。
    if let Some(scan) = state.active_scans.get_mut(key) {
        if let Some(next) = scan.revision.checked_add(1) {
            scan.revision = next;
        } else {
            // 版本不能回绕成扫描开始时的值。
            state.unknown_roots.insert(key.0.clone());
        }
    }
}

fn marker(root: &Path, session: &str) -> PathBuf {
    let hash = format!("{:x}", Sha256::digest(session.as_bytes()));
    root.join(format!("gap-{hash}.trace-gap"))
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 写入前复核 scoped root、`v3` 与设备根都不是链接。
///
/// 读取侧已经在每次打开文件前做这一层；写入侧如果只在最后打开文件时
/// 拒绝链接，仍可能先顺着父目录 junction 把内容写到受控根之外。
fn reject_linked_directory_layer(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_link_or_reparse(&metadata) => Err(io::Error::other(
            "Computer Use trace 目录层是链接或 reparse point",
        )),
        Ok(metadata) if !metadata.is_dir() => {
            Err(io::Error::other("Computer Use trace 目录层不是普通目录"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn prepare_root(root: &Path) -> io::Result<()> {
    reject_linked_directory_layer(root)?;
    if let Some(version_root) = root.parent() {
        reject_linked_directory_layer(version_root)?;
        if let Some(base) = version_root.parent() {
            reject_linked_directory_layer(base)?;
        }
    }
    fs::create_dir_all(root)?;
    reject_linked_directory_layer(root)?;
    if let Some(version_root) = root.parent() {
        reject_linked_directory_layer(version_root)?;
        if let Some(base) = version_root.parent() {
            reject_linked_directory_layer(base)?;
        }
    }

    let canonical_root = fs::canonicalize(root)?;
    let version_root = root
        .parent()
        .ok_or_else(|| io::Error::other("Computer Use trace root 缺少父目录"))?;
    let canonical_version = fs::canonicalize(version_root)?;
    if canonical_root == canonical_version || !canonical_root.starts_with(&canonical_version) {
        return Err(io::Error::other("Computer Use trace root 越过受控目录"));
    }
    Ok(())
}

fn reject_linked_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_link_or_reparse(&metadata) => Err(io::Error::other(
            "Computer Use trace 文件是链接或 reparse point",
        )),
        Ok(metadata) if !metadata.is_file() => {
            Err(io::Error::other("Computer Use trace 文件不是普通文件"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
enum Job {
    Gap(Key),
    Append {
        key: Key,
        path: PathBuf,
        data: Vec<u8>,
    },
    #[cfg(test)]
    Flush(mpsc::Sender<()>),
    #[cfg(test)]
    Block(mpsc::Sender<()>, mpsc::Receiver<()>),
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn sender() -> &'static Option<mpsc::SyncSender<Job>> {
    static SENDER: OnceLock<Option<mpsc::SyncSender<Job>>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        match std::thread::Builder::new()
            .name("computer-trace-writer".into())
            .spawn(move || worker(rx))
        {
            Ok(_) => Some(tx),
            Err(error) => {
                tracing::warn!(%error, "无法启动 trace 后台写入线程");
                None
            }
        }
    })
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn persist_gap(key: &Key) -> io::Result<()> {
    prepare_root(&key.0)?;
    let path = marker(&key.0, &key.1);
    reject_linked_file(&path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    match options.open(&path) {
        Ok(file) => file.sync_all(),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.is_file() && !is_link_or_reparse(&metadata) {
                Ok(())
            } else {
                Err(io::Error::other("不完整标记不是普通文件"))
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn worker(rx: mpsc::Receiver<Job>) {
    for job in rx {
        match job {
            Job::Gap(key) => {
                if let Err(error) = persist_gap(&key) {
                    lock().unknown_roots.insert(key.0);
                    tracing::warn!(%error, "不完整标记无法持久化");
                }
            }
            Job::Append { key, path, data } => {
                let result = (|| -> io::Result<()> {
                    prepare_root(&key.0)?;
                    reject_linked_file(&path)?;
                    let mut options = OpenOptions::new();
                    options.create(true).append(true);
                    #[cfg(windows)]
                    options.custom_flags(
                        windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
                    );
                    #[cfg(unix)]
                    options.custom_flags(libc::O_NOFOLLOW);
                    let mut file = options.open(&path)?;
                    file.write_all(&data)?;
                    file.write_all(b"\n")?;
                    file.sync_data()?;
                    Ok(())
                })();
                {
                    let mut state = lock();
                    if let Some(count) = state.pending.get_mut(&key) {
                        *count -= 1;
                        if *count == 0 {
                            state.pending.remove(&key);
                        }
                    }
                    if result.is_err() {
                        if state.memory_gaps.len() < MAX_TRACKED_SESSIONS
                            || state.memory_gaps.contains(&key)
                        {
                            state.memory_gaps.insert(key.clone());
                        } else {
                            state.unknown_roots.insert(key.0.clone());
                        }
                    }
                    bump(&mut state, &key);
                }
                if let Err(error) = result {
                    tracing::warn!(%error, "Computer Use trace 写入失败");
                    if let Err(error) = persist_gap(&key) {
                        lock().unknown_roots.insert(key.0);
                        tracing::warn!(%error, "不完整标记无法持久化");
                    }
                }
            }
            #[cfg(test)]
            Job::Flush(done) => {
                let _ = done.send(());
            }
            #[cfg(test)]
            Job::Block(started, resume) => {
                let _ = started.send(());
                let _ = resume.recv_timeout(std::time::Duration::from_secs(2));
            }
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn queue(job: Job) -> bool {
    let root = match &job {
        Job::Gap(key) | Job::Append { key, .. } => Some(key.0.clone()),
        #[cfg(test)]
        Job::Flush(_) | Job::Block(_, _) => None,
    };
    if sender().as_ref().is_some_and(|tx| tx.try_send(job).is_ok()) {
        true
    } else {
        if let Some(root) = root {
            lock().unknown_roots.insert(root);
        }
        tracing::error!("Computer Use 审计队列已满或不可用；历史完整性无法确认");
        false
    }
}

/// 同步标记内存后才排队；无论磁盘或历史扫描多慢，都不在 Tokio poll 中等待。
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) fn mark(root: Option<&Path>, session: &str) {
    let Some(root) = root else {
        // 没有根目录时并没有一条可以归到某个主体目录的审计写入。
        // 若调用方连 trace 根都没装配，读取入口本身会返回不可用；不要用
        // 这一个设备级错误污染所有账号已经验证完整的历史。
        tracing::error!("Computer Use 审计缺少 trace 根；本次记录未落盘");
        return;
    };
    let key = (root.to_path_buf(), session.to_owned());
    {
        let mut state = lock();
        if state.memory_gaps.len() < MAX_TRACKED_SESSIONS || state.memory_gaps.contains(&key) {
            state.memory_gaps.insert(key.clone());
        } else {
            state.unknown_roots.insert(root.to_path_buf());
        }
        bump(&mut state, &key);
    }
    let _ = queue(Job::Gap(key));
}

/// 写入完成前同一会话的历史拒绝声称完整；写失败则永久保留 gap。
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) fn append(path: Option<&Path>, session: &str, data: Vec<u8>) {
    let Some(path) = path else {
        mark(None, session);
        return;
    };
    let Some(root) = path.parent() else {
        mark(None, session);
        return;
    };
    let key = (root.to_path_buf(), session.to_owned());
    if data.len() > MAX_AUDIT_BYTES {
        tracing::error!("Computer Use 审计记录超过大小上限；历史完整性无法确认");
        mark(Some(root), session);
        return;
    }
    let tracking_full = {
        let mut state = lock();
        if !state.pending.contains_key(&key) && state.pending.len() >= MAX_TRACKED_SESSIONS {
            state.unknown_roots.insert(root.to_path_buf());
            true
        } else {
            *state.pending.entry(key.clone()).or_default() += 1;
            bump(&mut state, &key);
            false
        }
    };
    if tracking_full {
        tracing::error!("Computer Use 审计待写会话超过上限；历史完整性无法确认");
        return;
    }
    if !queue(Job::Append {
        key: key.clone(),
        path: path.to_path_buf(),
        data,
    }) {
        let mut state = lock();
        if let Some(count) = state.pending.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                state.pending.remove(&key);
            }
        }
        // 无法入队时无法保证 gap 落盘；进程内一律报不完整，不能声称恢复。
        state.unknown_roots.insert(root.to_path_buf());
    }
}

pub(crate) enum GapError {
    Incomplete,
    Io(io::Error),
}

/// 所有同一 session 的在途扫描共享版本；最后一个扫描结束才能回收条目。
struct ScanLease {
    key: Key,
    revision: u64,
}

impl ScanLease {
    fn acquire(key: Key) -> Result<Self, GapError> {
        let mut state = lock();
        if state.unknown_roots.contains(&key.0)
            || state.memory_gaps.contains(&key)
            || state.pending.contains_key(&key)
        {
            return Err(GapError::Incomplete);
        }
        if !state.active_scans.contains_key(&key)
            && state.active_scans.len() >= MAX_TRACKED_SESSIONS
        {
            // 并发查询过多只拒绝当前查询，不污染后续正常历史。
            return Err(GapError::Incomplete);
        }
        let scan = state.active_scans.entry(key.clone()).or_default();
        scan.readers = scan.readers.checked_add(1).ok_or(GapError::Incomplete)?;
        Ok(Self {
            key,
            revision: scan.revision,
        })
    }

    fn validate(&self) -> Result<(), GapError> {
        let state = lock();
        if state.unknown_roots.contains(&self.key.0)
            || state.memory_gaps.contains(&self.key)
            || state.pending.contains_key(&self.key)
            || state
                .active_scans
                .get(&self.key)
                .is_none_or(|scan| scan.revision != self.revision)
        {
            return Err(GapError::Incomplete);
        }
        Ok(())
    }
}

impl Drop for ScanLease {
    fn drop(&mut self) {
        let mut state = lock();
        if let Some(scan) = state.active_scans.get_mut(&self.key) {
            scan.readers -= 1;
            if scan.readers == 0 {
                state.active_scans.remove(&self.key);
            }
        } else {
            // 找不到在途租约意味着无法证明历史完整。
            state.unknown_roots.insert(self.key.0.clone());
        }
    }
}

fn check_marker(key: &Key) -> Result<(), GapError> {
    match fs::symlink_metadata(marker(&key.0, &key.1)) {
        Ok(_) => Err(GapError::Incomplete),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GapError::Io(error)),
    }
}

/// 仅短暂检查内存；扫描前后复核版本，不能在全局锁下做磁盘 I/O。
pub(crate) fn check<T, E: From<GapError>>(
    root: &Path,
    session: &str,
    read: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let key = (root.to_path_buf(), session.to_owned());
    let scan = ScanLease::acquire(key.clone())?;
    check_marker(&key)?;
    let value = read()?;
    check_marker(&key)?;
    scan.validate()?;
    Ok(value)
}

#[cfg(all(test, any(target_os = "windows", target_os = "macos")))]
pub(crate) fn flush_for_test() {
    let (tx, rx) = mpsc::channel();
    sender()
        .as_ref()
        .expect("审计线程")
        .send(Job::Flush(tx))
        .expect("测试刷新");
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .expect("后台审计必须完成");
}

#[cfg(all(test, any(target_os = "windows", target_os = "macos")))]
pub(crate) fn run_test_isolated(full_name: &str) -> bool {
    const CHILD_ENV: &str = "CORTEX_TEST_TRACE_GAP_CHILD";
    if std::env::var(CHILD_ENV).ok().as_deref() == Some(full_name) {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("测试程序路径"))
        .args([full_name, "--exact", "--nocapture"])
        .env(CHILD_ENV, full_name)
        .output()
        .expect("启动隔离的审计状态测试");
    assert!(
        output.status.success(),
        "隔离测试 {full_name} 失败：{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

#[cfg(all(test, any(target_os = "windows", target_os = "macos")))]
mod tests {
    use super::*;

    /// 这些测试有意堵塞全局写线程、填满队列或清空内存标记；必须使用独立
    /// 进程，避免并行测试把另一会话的真实完整性状态污染成 409。
    fn run_isolated(name: &str) -> bool {
        let full_name = format!("computer_trace_gap::tests::{name}");
        run_test_isolated(&full_name)
    }

    #[test]
    fn persisted_marker_survives_loss_of_in_process_memory() {
        if run_isolated("persisted_marker_survives_loss_of_in_process_memory") {
            return;
        }
        let dir = tempfile::tempdir().expect("隔离目录");
        mark(Some(dir.path()), "persisted-session");
        flush_for_test();
        {
            let mut state = lock();
            state.memory_gaps.clear();
            state.unknown_roots.clear();
        }
        let affected = check::<(), GapError>(dir.path(), "persisted-session", || Ok(()));
        assert!(
            matches!(affected, Err(GapError::Incomplete)),
            "新进程读取逻辑必须发现标记"
        );
        assert!(check::<(), GapError>(dir.path(), "other-session", || Ok(())).is_ok());
    }

    #[test]
    fn blocked_history_scan_does_not_block_gap_marking() {
        if run_isolated("blocked_history_scan_does_not_block_gap_marking") {
            return;
        }
        let dir = tempfile::tempdir().expect("隔离目录");
        let root = dir.path().to_path_buf();
        let scan_root = root.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let scan = std::thread::spawn(move || {
            check::<(), GapError>(&scan_root, "blocked-scan-session", || {
                started_tx.send(()).expect("扫描开始");
                resume_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .expect("扫描恢复");
                Ok(())
            })
        });
        started_rx.recv().expect("扫描已阻塞");
        let before = std::time::Instant::now();
        mark(Some(&root), "blocked-scan-session");
        assert!(
            before.elapsed() < std::time::Duration::from_secs(1),
            "标记不得等待阻塞中的文件扫描"
        );
        resume_tx.send(()).expect("恢复扫描");
        assert!(
            matches!(scan.join().expect("扫描线程"), Err(GapError::Incomplete)),
            "扫描期间出现新 gap 时不能报告完整"
        );
        flush_for_test();
    }

    #[test]
    fn blocked_disk_worker_does_not_block_marking_or_hide_incomplete_history() {
        if run_isolated("blocked_disk_worker_does_not_block_marking_or_hide_incomplete_history") {
            return;
        }
        let dir = tempfile::tempdir().expect("隔离目录");
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        assert!(queue(Job::Block(started_tx, resume_rx)), "测试屏障必须入队");
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("后台写入已阻塞");
        let before = std::time::Instant::now();
        mark(Some(dir.path()), "queued-gap-session");
        assert!(
            before.elapsed() < std::time::Duration::from_secs(1),
            "标记不能等待磁盘写入"
        );
        assert!(
            matches!(
                check::<(), GapError>(dir.path(), "queued-gap-session", || Ok(())),
                Err(GapError::Incomplete)
            ),
            "落盘前内存状态必须拒绝返回完整历史"
        );
        resume_tx.send(()).expect("恢复后台写入");
        flush_for_test();
    }

    #[test]
    fn full_queue_returns_immediately_and_fails_closed() {
        if run_isolated("full_queue_returns_immediately_and_fails_closed") {
            return;
        }
        let dir = tempfile::tempdir().expect("隔离目录");
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        assert!(queue(Job::Block(started_tx, resume_rx)), "测试屏障必须入队");
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("写线程已阻塞");
        let path = dir.path().join("bounded.jsonl");
        for index in 0..QUEUE_CAPACITY {
            append(Some(&path), &format!("session-{index}"), b"{}".to_vec());
        }
        let before = std::time::Instant::now();
        append(Some(&path), "overflow-session", b"{}".to_vec());
        assert!(
            before.elapsed() < std::time::Duration::from_secs(1),
            "队列满不能阻塞调用者"
        );
        assert!(
            matches!(
                check::<(), GapError>(dir.path(), "overflow-session", || Ok(())),
                Err(GapError::Incomplete)
            ),
            "被拒绝的审计必须显示历史不完整"
        );
        assert!(
            matches!(
                check::<(), GapError>(dir.path(), "other-session", || Ok(())),
                Err(GapError::Incomplete)
            ),
            "同一主体根下无法持久化溢出标记时必须保守失败"
        );
        let other_root = tempfile::tempdir().expect("另一主体根");
        assert!(
            check::<(), GapError>(other_root.path(), "other-session", || Ok(())).is_ok(),
            "一个主体根的队列溢出不能污染另一个主体根"
        );
        resume_tx.send(()).expect("恢复写入");
        flush_for_test();
        assert!(
            matches!(
                check::<(), GapError>(dir.path(), "overflow-session", || Ok(())),
                Err(GapError::Incomplete)
            ),
            "队列排空不能清除溢出不完整状态"
        );
    }

    #[test]
    fn oversized_audit_is_not_enqueued_as_complete() {
        if run_isolated("oversized_audit_is_not_enqueued_as_complete") {
            return;
        }
        let dir = tempfile::tempdir().expect("隔离目录");
        append(
            Some(&dir.path().join("oversized.jsonl")),
            "oversized-session",
            vec![b'x'; MAX_AUDIT_BYTES + 1],
        );
        assert!(
            matches!(
                check::<(), GapError>(dir.path(), "oversized-session", || Ok(())),
                Err(GapError::Incomplete)
            ),
            "超限记录不能让历史看起来完整"
        );
        flush_for_test();
    }

    #[tokio::test]
    async fn more_than_4096_successful_sessions_remain_queryable() {
        if run_isolated("more_than_4096_successful_sessions_remain_queryable") {
            return;
        }
        let dir = tempfile::tempdir().expect("隔离目录");
        let path = dir.path().join("successful.jsonl");
        let count = MAX_TRACKED_SESSIONS + 17;
        for index in 0..count {
            let session = format!("successful-{index}");
            let trace = serde_json::json!({
                "schema_version": 1,
                "run_id": "run-safe",
                "session_id": session,
                "policy_version": "test",
                "app": {"kind": "windows_executable", "stable_id": "app", "publisher_verified": true},
                "redaction": {
                    "raw_screenshot_frames": "hash_only", "typed_text": "omitted",
                    "ax_secure_values": "omitted", "cookies_tokens_passwords": "omitted",
                    "clipboard_contents": "omitted"
                },
                "events": [{
                    "sequence": 1, "type": "observation",
                    "observation": {
                        "frame_hash": "frame", "window_ref": "window",
                        "accessibility_summary_hash": null, "screenshot_hash": null
                    }
                }],
                "outcome": "observed_only"
            });
            append(
                Some(&path),
                &session,
                serde_json::to_vec(&trace).expect("有效 trace"),
            );
            if index % 32 == 31 {
                flush_for_test();
            }
        }
        flush_for_test();
        for index in [0, MAX_TRACKED_SESSIONS - 1, MAX_TRACKED_SESSIONS, count - 1] {
            let session = format!("successful-{index}");
            let response = crate::computer_traces::history(
                Some(dir.path().to_path_buf()),
                session.clone(),
                false,
            )
            .await;
            assert_eq!(
                response.status(),
                axum::http::StatusCode::OK,
                "第 {index} 个正常会话必须仍可查询"
            );
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("响应");
            let history: cortex_proto::dto::ComputerUseTraceHistory =
                serde_json::from_slice(&body).expect("旧协议");
            assert_eq!(history.traces.len(), 1, "只返回目标会话");
            assert_eq!(history.traces[0].session_id, session);
        }
    }

    #[test]
    fn active_scan_version_survives_other_scan_reclamation_and_concurrent_write() {
        if run_isolated("active_scan_version_survives_other_scan_reclamation_and_concurrent_write")
        {
            return;
        }
        let dir = tempfile::tempdir().expect("隔离目录");
        let root = dir.path().to_path_buf();
        let (started_tx, started_rx) = mpsc::channel();
        let mut releases = Vec::new();
        let mut scans = Vec::new();
        for _ in 0..2 {
            let (resume_tx, resume_rx) = mpsc::channel();
            releases.push(resume_tx);
            let started = started_tx.clone();
            let scan_root = root.clone();
            scans.push(std::thread::spawn(move || {
                check::<(), GapError>(&scan_root, "active-session", || {
                    started.send(()).expect("扫描开始");
                    resume_rx
                        .recv_timeout(std::time::Duration::from_secs(15))
                        .expect("扫描恢复");
                    Ok(())
                })
            }));
        }
        for _ in 0..2 {
            started_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("扫描已进入");
        }
        append(
            Some(&root.join("active.jsonl")),
            "active-session",
            b"{}".to_vec(),
        );
        flush_for_test();
        releases.remove(0).send(()).expect("恢复第一个扫描");
        assert!(
            matches!(
                scans.remove(0).join().expect("扫描线程"),
                Err(GapError::Incomplete)
            ),
            "扫描期间写入完成也必须拒绝旧结果"
        );
        for index in 0..(MAX_TRACKED_SESSIONS + 17) {
            let session = format!("reclaimed-{index}");
            assert!(
                check::<(), GapError>(&root, &session, || Ok(())).is_ok(),
                "已结束扫描应可回收"
            );
        }
        {
            let state = lock();
            let key = (root.clone(), "active-session".to_owned());
            assert_eq!(
                state.active_scans.get(&key).map(|scan| scan.readers),
                Some(1),
                "另一个扫描在途时不得回收其版本"
            );
        }
        releases.remove(0).send(()).expect("恢复第二个扫描");
        assert!(
            matches!(
                scans.remove(0).join().expect("扫描线程"),
                Err(GapError::Incomplete)
            ),
            "其他扫描条目被回收后仍须检测并发写入"
        );
        assert!(
            check::<(), GapError>(&root, "active-session", || Ok(())).is_ok(),
            "旧扫描结束后，新扫描可读取已完成的写入"
        );
    }

    #[test]
    fn active_scan_limit_recovers_when_a_lease_is_released() {
        if run_isolated("active_scan_limit_recovers_when_a_lease_is_released") {
            return;
        }
        let dir = tempfile::tempdir().expect("隔离目录");
        let mut leases = Vec::new();
        for index in 0..MAX_TRACKED_SESSIONS {
            let key = (dir.path().to_path_buf(), format!("active-{index}"));
            match ScanLease::acquire(key) {
                Ok(lease) => leases.push(lease),
                Err(_) => panic!("并发扫描上限内必须可进入"),
            }
        }
        let extra = (dir.path().to_path_buf(), "extra".to_owned());
        assert!(
            matches!(ScanLease::acquire(extra.clone()), Err(GapError::Incomplete)),
            "超限只拒绝当前扫描"
        );
        drop(leases.pop());
        assert!(ScanLease::acquire(extra).is_ok(), "释放租约后容量必须恢复");
        assert!(
            !lock().unknown_roots.contains(dir.path()),
            "容量满不能永久污染当前主体的历史状态"
        );
    }
}
