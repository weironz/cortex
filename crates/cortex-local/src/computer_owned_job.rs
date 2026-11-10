// SPDX-License-Identifier: Apache-2.0
// Adapted from OpenAI Codex, codex-rs/utils/pty/src/win/job.rs.
// Copyright OpenAI. Licensed under the Apache License, Version 2.0.
// 来源、固定版本及适配差异见仓库 NOTICE；Apache-2.0 全文见根 LICENSE。
//! 只管理本进程创建的输入运行时；不接受外部 PID，不提供 breakaway 或保留子孙。
//! 原 Job 的排空证明只能由显式有界查询产生；Drop 只尽力终止，绝不签发证明。

use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HANDLE, WAIT_FAILED, WAIT_OBJECT_0};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, WaitForSingleObject,
};

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtResumeProcess(process: HANDLE) -> i32;
}

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const FAILED_SPAWN_CLEANUP: Duration = Duration::from_secs(2);

#[derive(Debug, Eq, PartialEq)]
enum Phase {
    Open,
    Sealed,
    ProofIssued,
    ProofConsumed,
}

/// 句柄不公开、不可复制；spawn 与 seal 共用锁，证明产生后不能再次加入进程。
#[derive(Debug)]
pub(crate) struct OwnedJob {
    handle: OwnedHandle,
    identity: Arc<()>,
    phase: Mutex<Phase>,
    roots: Mutex<Vec<OwnedHandle>>,
    failed_children: Mutex<Vec<Child>>,
}

/// 绑定同一个原 Job 实例，不能 Clone、从 PID 重建或由 Drop 产生。
#[derive(Debug)]
#[must_use = "关闭成功前必须由原 Job 消费排空证明"]
pub(crate) struct Drained {
    identity: Arc<()>,
}

impl OwnedJob {
    #[cfg(test)]
    pub(crate) fn with_phase_locked_for_test<T>(&self, operation: impl FnOnce() -> T) -> T {
        let _phase = self.phase.lock().expect("测试需持有真实 Job phase 锁");
        operation()
    }

    pub(crate) fn is_open(&self) -> bool {
        self.phase
            .try_lock()
            .is_ok_and(|phase| *phase == Phase::Open)
    }
    /// 仅供持有同一原对象的清理路径幂等复用；不是给调用方签发第二份排空证明。
    pub(crate) fn is_confirmed_drained(&self) -> bool {
        self.phase
            .try_lock()
            .is_ok_and(|phase| *phase == Phase::ProofConsumed)
    }
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: 无继承、无外部名称，只创建本组件拥有的 Job。
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW 返回的新句柄在这里唯一接管。
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: 结构与长度匹配；故意不设置任何 breakaway 标志。
        let configured = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle,
            identity: Arc::new(()),
            phase: Mutex::new(Phase::Open),
            roots: Mutex::new(Vec::new()),
            failed_children: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn spawn_contained(&self, command: &mut Command) -> io::Result<Child> {
        self.spawn_with_assignment(command, |child| {
            // SAFETY: 两个句柄分别由本 Job 与刚创建、尚未运行的 Child 持有。
            if unsafe {
                AssignProcessToJobObject(self.handle.as_raw_handle(), child.as_raw_handle())
            } == 0
            {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })
    }

    /// 只查询由内核给出的 pipe peer；不会接管或终止外部进程。
    pub(crate) fn contains_process(&self, process: BorrowedHandle<'_>) -> io::Result<bool> {
        let mut contained = 0;
        // SAFETY: 原 Job 和借用进程句柄均存活，只查询 membership。
        if unsafe {
            windows_sys::Win32::System::JobObjects::IsProcessInJob(
                process.as_raw_handle(),
                self.handle.as_raw_handle(),
                &mut contained,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(contained != 0)
    }

    fn prune_roots(roots: &mut Vec<OwnedHandle>) -> io::Result<()> {
        let mut index = 0;
        while index < roots.len() {
            // SAFETY: 仅检查本组件已保存的原句柄；退出后关闭，不随每次 CLI 累积句柄。
            match unsafe { WaitForSingleObject(roots[index].as_raw_handle(), 0) } {
                WAIT_OBJECT_0 => {
                    roots.swap_remove(index);
                }
                WAIT_FAILED => return Err(io::Error::last_os_error()),
                _ => index += 1,
            }
        }
        Ok(())
    }

    fn spawn_with_assignment(
        &self,
        command: &mut Command,
        assign: impl FnOnce(&Child) -> io::Result<()>,
    ) -> io::Result<Child> {
        let phase = self
            .phase
            .lock()
            .map_err(|_| io::Error::other("Job 生命周期锁不可用"))?;
        if *phase != Phase::Open {
            return Err(io::Error::other("Job 已密封，禁止创建或恢复旧运行时"));
        }
        let mut roots = self
            .roots
            .lock()
            .map_err(|_| io::Error::other("Job 根句柄表不可用"))?;
        Self::prune_roots(&mut roots)?;
        // CREATE_NO_WINDOW 隐藏控制台，不改变 Chromium headful 窗口设置。
        command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
        let child = command.spawn()?;
        if let Err(error) = assign(&child) {
            return Err(self.reject_spawn(child, "assign", error));
        }
        let root = match child.as_handle().try_clone_to_owned() {
            Ok(root) => root,
            Err(error) => return Err(self.reject_spawn(child, "保留原进程句柄", error)),
        };
        // SAFETY: Child 原句柄仍活着，且已成功加入禁止 breakaway 的原 Job。
        let status = unsafe { NtResumeProcess(child.as_raw_handle()) };
        if status < 0 {
            return Err(self.reject_spawn(
                child,
                "resume",
                io::Error::other(format!("NTSTATUS {status:#x}")),
            ));
        }
        roots.push(root);
        Ok(child)
    }

    fn reject_spawn(&self, mut child: Child, stage: &str, cause: io::Error) -> io::Error {
        let error = stop_failed_spawn(&mut child, stage, cause);
        if !matches!(child.try_wait(), Ok(Some(_))) {
            // assign 失败的进程尚不在 Job 中，必须保留原 Child；不能只保管 Job。
            // 中毒 guard 仅用于清理保管，不恢复任何派发或证明权限。
            match self.failed_children.lock() {
                Ok(mut children) => children.push(child),
                Err(poisoned) => poisoned.into_inner().push(child),
            }
        }
        error
    }

    pub(crate) fn terminate_and_drain(&self, timeout: Duration) -> io::Result<Drained> {
        self.drain_with_query(timeout, || self.active_processes())
    }

    fn drain_with_query(
        &self,
        timeout: Duration,
        mut query: impl FnMut() -> io::Result<u32>,
    ) -> io::Result<Drained> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| io::Error::other("Job 排空预算溢出"))?;
        let mut phase = loop {
            match self.phase.try_lock() {
                Ok(phase) => break phase,
                Err(TryLockError::Poisoned(_)) => {
                    return Err(io::Error::other("Job 生命周期锁不可用"));
                }
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(
                        POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Job 启动临界区在排空预算内未退出",
                    ));
                }
            }
        };
        if matches!(*phase, Phase::ProofIssued | Phase::ProofConsumed) {
            return Err(io::Error::other("原 Job 的排空证明已签发，不能重新签发"));
        }
        *phase = Phase::Sealed;
        self.terminate()?;
        let mut failed = self
            .failed_children
            .lock()
            .map_err(|_| io::Error::other("失败创建保管锁中毒"))?;
        for child in failed.iter_mut() {
            let _ = child.kill();
        }
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "原 Job 在预算内未取得成员归零证明",
                ));
            }
            let active = query()?;
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Job accounting 响应超过排空预算",
                ));
            }
            // Windows accounting 可先归零，根进程句柄稍后才置为已退出。
            // 等待这些原句柄也共用同一 deadline，不能重置或另加预算。
            let mut roots_exited = true;
            if active == 0 {
                let roots = self
                    .roots
                    .lock()
                    .map_err(|_| io::Error::other("Job 根句柄表不可用"))?;
                for root in roots.iter() {
                    // SAFETY: 仅查询 spawn 时保留的精确自有根进程句柄。
                    match unsafe { WaitForSingleObject(root.as_raw_handle(), 0) } {
                        WAIT_OBJECT_0 => {}
                        WAIT_FAILED => return Err(io::Error::last_os_error()),
                        _ => roots_exited = false,
                    }
                }
            }
            let mut failed_exited = true;
            for child in failed.iter_mut() {
                if child.try_wait()?.is_none() {
                    failed_exited = false;
                }
            }
            if active == 0 && roots_exited && failed_exited {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "根进程退出确认超过 Job 总预算",
                    ));
                }
                *phase = Phase::ProofIssued;
                return Ok(Drained {
                    identity: self.identity.clone(),
                });
            }
            std::thread::sleep(
                POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    pub(crate) fn accept_drained(&self, proof: Drained) -> io::Result<()> {
        let mut phase = self
            .phase
            .lock()
            .map_err(|_| io::Error::other("Job 生命周期锁不可用"))?;
        if !Arc::ptr_eq(&self.identity, &proof.identity) || *phase != Phase::ProofIssued {
            return Err(io::Error::other("排空证明不属于这个原 Job 实例"));
        }
        *phase = Phase::ProofConsumed;
        Ok(())
    }

    fn active_processes(&self) -> io::Result<u32> {
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: 只查询本组件自始持有的原 Job，不通过 PID/名称重新打开。
        let ok = unsafe {
            QueryInformationJobObject(
                self.handle.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut accounting).cast(),
                std::mem::size_of_val(&accounting) as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(accounting.ActiveProcesses)
        }
    }

    fn terminate(&self) -> io::Result<()> {
        // SAFETY: 仅终止这个 Job 内、由严格 suspended/assign 路径创建的自有进程。
        if unsafe { TerminateJobObject(self.handle.as_raw_handle(), 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for OwnedJob {
    fn drop(&mut self) {
        // 只清理；即使 terminate 返回成功，也不能生成 Drained 或报告排空。
        let _ = self.terminate();
        let failed = match self.failed_children.get_mut() {
            Ok(children) => children,
            Err(poisoned) => poisoned.into_inner(),
        };
        for child in failed {
            let _ = child.kill();
        }
    }
}

fn stop_failed_spawn(child: &mut Child, stage: &str, cause: io::Error) -> io::Error {
    let killed = child.kill();
    let deadline = Instant::now() + FAILED_SPAWN_CLEANUP;
    let reaped = loop {
        match child.try_wait() {
            Ok(Some(_)) => break Ok(()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "挂起子进程退出未确认",
                ));
            }
            Err(error) => break Err(error),
        }
    };
    io::Error::other(format!(
        "Job {stage} 失败，未执行 resume：{cause}；终止={killed:?}，回收={reaped:?}"
    ))
}

#[cfg(test)]
#[path = "computer_owned_job_test.rs"]
mod tests;
