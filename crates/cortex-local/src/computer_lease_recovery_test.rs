//! 仅测试私有窗口条目/原事件的保管与释放，不伪造 Core committed 或 Job 排空证明。
//! 公共 release 的跨模块真实证明路径由主线 Host 恢复集成测试覆盖。
use super::*;

fn window(label: &str) -> String {
    format!("window-recovery-{label}-{}", cortex_core::Id::new())
}

fn owns(registry: &ComputerLeaseRegistry, window: &str, run: &str) {
    assert_eq!(
        registry.owner_of(window).as_deref(),
        Some(run),
        "不能变更未被证明覆盖的窗口 owner"
    );
}

#[test]
fn binding_requires_existing_windows_and_clones_preserve_original_identity() {
    let registry = Arc::new(ComputerLeaseRegistry::new());
    assert!(
        registry.bind_recovery("a").is_err(),
        "裸 run 名称不能重造窗口能力"
    );
    let target = window("binding");
    registry.acquire_window("a", &target).unwrap();
    assert!(
        registry.bind_recovery("b").is_err(),
        "其他 run 不能借用已有窗口"
    );
    let binding = registry.bind_recovery("a").unwrap();
    assert_eq!(binding, binding.clone(), "Clone 必须保留同一原对象");
    assert_eq!(
        binding,
        registry.bind_recovery("a").unwrap(),
        "不变条目的重复绑定身份相同"
    );
}

#[test]
fn wrong_registry_and_run_cannot_release_or_quarantine_another_owner() {
    let first = Arc::new(ComputerLeaseRegistry::new());
    let second = Arc::new(ComputerLeaseRegistry::new());
    let a = window("registry-a");
    let b = window("registry-b");
    let foreign = window("registry-foreign");
    first.acquire_window("a", &a).unwrap();
    first.acquire_window("b", &b).unwrap();
    second.acquire_window("a", &foreign).unwrap();
    let binding = first.bind_recovery("a").unwrap();
    let mut wrong = binding.clone();
    wrong.registry = Arc::clone(&second);
    assert!(
        wrong.release_original_entries().is_err(),
        "原 incarnation 不能移植到另一 registry"
    );
    let mut wrong = binding.clone();
    wrong.run_id = "b".into();
    assert!(
        wrong.release_original_entries().is_err(),
        "不能按另一个 run 的字符串释放窗口"
    );
    owns(&first, &a, "a");
    owns(&first, &b, "b");
    owns(&second, &foreign, "a");
    assert!(
        first.quarantined_runs.lock().unwrap().is_empty(),
        "错误绑定不能隔离其他 owner"
    );
    assert!(
        second.quarantined_runs.lock().unwrap().is_empty(),
        "错误 registry 不得被修改"
    );
}

#[test]
fn release_without_prior_quarantine_keeps_old_admission_revoked_and_other_runs_untouched() {
    let registry = Arc::new(ComputerLeaseRegistry::new());
    let first = window("release-first");
    let second = window("release-second");
    let other = window("release-other");
    registry.acquire_window("old", &first).unwrap();
    registry.acquire_window("old", &second).unwrap();
    registry.acquire_window("other", &other).unwrap();
    let binding = registry.bind_recovery("old").unwrap();
    let other_handle = registry.os_mutexes.lock().unwrap()[&other].load(Ordering::Acquire);
    assert!(
        !registry.quarantined_runs.lock().unwrap().contains("old"),
        "覆盖无 dispatch 的未预先隔离情形"
    );
    binding.release_original_entries().unwrap();
    assert!(registry.owner_of(&first).is_none());
    assert!(registry.owner_of(&second).is_none());
    owns(&registry, &other, "other");
    assert_eq!(
        registry.os_mutexes.lock().unwrap()[&other].load(Ordering::Acquire),
        other_handle,
        "不关闭其他 run 的事件"
    );
    assert!(
        registry.acquire_window("old", &window("late")).is_err(),
        "旧 run 即使换窗口也必须永久禁入"
    );
    assert!(
        !registry.try_release_run("old"),
        "普通 cleanup 不能抹掉旧 run 的终态隔离"
    );
    let external = ComputerLeaseRegistry::new();
    external
        .acquire_window("new", &first)
        .expect("绑定仍存活时原事件也必须确实关闭");
    external.release_run("new");
    registry
        .acquire_window("new", &first)
        .expect("新 run 可取得已释放的同一窗口");
    registry.acquire_window("new", &second).unwrap();
}

#[test]
fn retry_and_late_callbacks_never_close_the_new_owner_event() {
    let registry = Arc::new(ComputerLeaseRegistry::new());
    let target = window("retry");
    registry.acquire_window("old", &target).unwrap();
    let binding = registry.bind_recovery("old").unwrap();
    registry.quarantine_run("old");
    binding.release_original_entries().unwrap();
    registry.acquire_window("new", &target).unwrap();
    let handle = registry.os_mutexes.lock().unwrap()[&target].load(Ordering::Acquire);
    binding
        .clone()
        .release_original_entries()
        .expect("同一原条目的释放重试可幂等");
    registry.quarantine_run("old");
    registry.release_window("old", &target);
    registry.release_run("old");
    assert!(
        registry.acquire_window("old", &target).is_err(),
        "迟到 callback 不能取回原窗口"
    );
    assert!(
        registry
            .acquire_window("old", &window("retry-new-window"))
            .is_err(),
        "旧 run 不能从另一窗口恢复 admission"
    );
    owns(&registry, &target, "new");
    assert_eq!(
        registry.os_mutexes.lock().unwrap()[&target].load(Ordering::Acquire),
        handle,
        "不能因原句柄数值重用关闭新事件"
    );
    assert!(
        ComputerLeaseRegistry::new()
            .acquire_window("third", &target)
            .is_err()
            || !cfg!(windows),
        "Windows 新 owner 的实际事件仍排他"
    );
}

#[test]
fn same_run_text_after_normal_release_gets_a_different_incarnation() {
    let registry = Arc::new(ComputerLeaseRegistry::new());
    let target = window("incarnation");
    registry.acquire_window("run", &target).unwrap();
    let old = registry.bind_recovery("run").unwrap();
    registry.release_run("run");
    registry.acquire_window("run", &target).unwrap();
    let current = registry.bind_recovery("run").unwrap();
    assert_ne!(
        old, current,
        "不能按同名 run/window 或复用的句柄值重建 incarnation"
    );
    assert!(
        old.release_original_entries().is_err(),
        "旧绑定不能释放新 incarnation"
    );
    owns(&registry, &target, "run");
    assert!(
        !registry.quarantined_runs.lock().unwrap().contains("run"),
        "旧绑定不能隔离另一 incarnation"
    );
}

#[test]
fn an_extra_unbound_window_fails_before_any_event_is_closed() {
    let registry = Arc::new(ComputerLeaseRegistry::new());
    let first = window("unbound-first");
    let second = window("unbound-second");
    registry.acquire_window("old", &first).unwrap();
    let binding = registry.bind_recovery("old").unwrap();
    registry.acquire_window("old", &second).unwrap();
    assert!(
        binding.release_original_entries().is_err(),
        "不完整绑定不能部分宣称成功"
    );
    owns(&registry, &first, "old");
    owns(&registry, &second, "old");
    assert!(
        !binding.windows[0].1.closed.load(Ordering::Acquire),
        "必须先校验全体条目再关闭事件"
    );
    assert!(
        registry
            .acquire_window("old", &window("unbound-late"))
            .is_err(),
        "失败后原 run 仍须禁入"
    );
}

#[test]
fn partially_closed_original_events_are_retained_and_not_closed_twice() {
    let registry = Arc::new(ComputerLeaseRegistry::new());
    let first = window("partial-first");
    let second = window("partial-second");
    registry.acquire_window("old", &first).unwrap();
    registry.acquire_window("old", &second).unwrap();
    let binding = registry.bind_recovery("old").unwrap();
    {
        let handles = registry.os_mutexes.lock().unwrap();
        let recovery = registry.recovery.lock().unwrap();
        close_original_event(&recovery.windows[&first], &handles[&first]).unwrap();
    }
    owns(&registry, &first, "old");
    binding
        .release_original_entries()
        .expect("部分原事件已关也能继续，不能 double-close");
    assert!(registry.owner_of(&first).is_none());
    assert!(registry.owner_of(&second).is_none());
    registry.acquire_window("new", &first).unwrap();
    registry.acquire_window("new", &second).unwrap();
}

#[cfg(windows)]
#[test]
fn replacement_handle_is_rejected_without_closing_either_owned_event() {
    let registry = Arc::new(ComputerLeaseRegistry::new());
    let target = window("handle-original");
    let replacement = window("handle-replacement");
    registry.acquire_window("old", &target).unwrap();
    let binding = registry.bind_recovery("old").unwrap();
    let other = acquire_windows_mutex(&replacement).expect("只创建本测试的替换事件");
    let original = registry.os_mutexes.lock().unwrap()[&target]
        .swap(other.load(Ordering::Acquire), Ordering::AcqRel);
    let result = binding.release_original_entries();
    // 先恢复测试自己暂时移走的句柄，避免断言失败遗失任何一个自有事件。
    registry.os_mutexes.lock().unwrap()[&target].store(original, Ordering::Release);
    close_windows_mutex(other.load(Ordering::Acquire));
    assert!(result.is_err(), "不能按窗口名误关被替换的实际句柄");
    owns(&registry, &target, "old");
    assert!(
        !binding.windows[0].1.closed.load(Ordering::Acquire),
        "原事件仍被精确保管"
    );
    binding
        .release_original_entries()
        .expect("恢复原事件后可继续精确释放");
}
