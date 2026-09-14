//! Live process-group handles for in-flight turn work.
//!
//! The durable record of a cancellation is the database (`run_controls.cancel_requested_at`).
//! That tells the agent loop to stop *between* steps, but a turn blocked inside a long-running
//! foreground command, an edit's diagnostics command, or an LSP invocation is not between steps.
//! This module is the kernel handle that lets an explicit cancel actually terminate the process
//! group the turn is still waiting on, instead of only flagging it for after the command returns.
//!
//! It is best-effort and in-memory by design: a process restart loses the handles, and the
//! durable intent still forces the turn to `interrupted` without replaying any side effect.
//!
//! Registration is cancellation-aware. `terminate` *seals* the request before it drains, so a group
//! spawned and registered after the one-shot terminate ran — the spawn->register race in
//! `run_capped_for` — is killed on sight instead of being quietly remembered. The seal is bounded
//! and in-memory like the rest of this module.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};

/// One lock guards both what is live and which requests are already cancelled, so a cancellation
/// and a late registration can never interleave into a surviving group.
struct Registry {
    live: HashMap<String, Vec<i32>>,
    /// Requests whose cancellation already ran. A group spawned after that point must be killed on
    /// sight, because the one-shot `terminate` it missed will never run again.
    sealed: HashSet<String>,
    sealed_order: VecDeque<String>,
}

/// A group can only be spawned in the tiny spawn->register gap of one tool step, so an old seal is
/// irrelevant long before it is evicted; the bound just keeps a long-lived process from remembering
/// every cancellation forever.
const MAX_SEALED: usize = 4096;

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        Mutex::new(Registry {
            live: HashMap::new(),
            sealed: HashSet::new(),
            sealed_order: VecDeque::new(),
        })
    })
}

/// Registers a live process-group id under a request for as long as the guard is held.
/// Dropping it deregisters, so a reused pid is never signalled by a later cancellation.
///
/// If the request was already cancelled, the group is signalled immediately and not remembered: it
/// was spawned after the cancel path ran and would otherwise outlive the cancelled turn. The pid is
/// the child this process still owns (it has not been reaped), so this cannot hit a reused pid.
pub struct GroupGuard {
    request: String,
    pgid: i32,
    registered: bool,
}

pub fn register(request: &str, pgid: i32) -> GroupGuard {
    if pgid <= 0 {
        return GroupGuard { request: request.to_string(), pgid, registered: false };
    }
    let late = {
        let mut reg = registry().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if reg.sealed.contains(request) {
            true
        } else {
            reg.live.entry(request.to_string()).or_default().push(pgid);
            false
        }
    };
    if late {
        signal_group(pgid);
        return GroupGuard { request: request.to_string(), pgid, registered: false };
    }
    GroupGuard { request: request.to_string(), pgid, registered: true }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        if let Ok(mut reg) = registry().lock() {
            if let Some(list) = reg.live.get_mut(&self.request) {
                list.retain(|held| *held != self.pgid);
                if list.is_empty() {
                    reg.live.remove(&self.request);
                }
            }
        }
    }
}

/// Remember `request` as cancelled, so a registration that lands after the drain is killed at once.
fn seal(reg: &mut Registry, request: &str) {
    if reg.sealed.insert(request.to_string()) {
        reg.sealed_order.push_back(request.to_string());
        while reg.sealed_order.len() > MAX_SEALED {
            if let Some(oldest) = reg.sealed_order.pop_front() {
                reg.sealed.remove(&oldest);
            }
        }
    }
}

/// Remove and return every live process group registered for `request`. Pure bookkeeping, split out
/// so the signalling path is the only thing that touches the kernel (and stays testable).
fn drain(reg: &mut Registry, request: &str) -> Vec<i32> {
    reg.live.remove(request).unwrap_or_default()
}

/// Signal one process group. Negative pid targets the whole group (leader + every descendant that
/// inherited it). Best effort: ESRCH simply means it already exited.
#[cfg(unix)]
fn signal_group(pgid: i32) {
    if pgid <= 0 {
        return;
    }
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
        libc::kill(-pgid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn signal_group(_pgid: i32) {}

/// Terminate every live process group registered for `request` and forget them, so a cancel never
/// signals a pid the OS has since reused. Returns how many groups were signalled. The request is
/// sealed first, so a group that registers only after this returns is still stopped.
pub fn terminate(request: &str) -> usize {
    let drained = {
        let mut reg = registry().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        seal(&mut reg, request);
        drain(&mut reg, request)
    };
    for pgid in &drained {
        signal_group(*pgid);
    }
    drained.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(request: &str) -> Option<Vec<i32>> {
        registry().lock().unwrap().live.get(request).cloned()
    }

    #[test]
    fn dropping_a_guard_deregisters_its_group() {
        let guard = register("req", 4242);
        assert_eq!(live("req"), Some(vec![4242]));
        drop(guard);
        assert!(live("req").is_none());
    }

    #[test]
    fn draining_never_touches_another_request_and_is_idempotent() {
        let _a = register("a", 11);
        let _b = register("b", 22);
        let first = drain(&mut registry().lock().unwrap(), "a");
        assert_eq!(first, vec![11]);
        assert_eq!(drain(&mut registry().lock().unwrap(), "a"), Vec::<i32>::new());
        assert!(live("a").is_none());
        assert_eq!(live("b"), Some(vec![22]));
    }

    #[test]
    fn a_non_positive_group_is_never_registered() {
        let _guard = register("zero", 0);
        assert!(live("zero").is_none());
    }

    /// P14-REVIEW-02: a group spawned after the cancel path ran must still die. The one-shot
    /// `terminate` drained nothing, so without coordination the late `register` would let the group
    /// run to completion and produce its delayed side effect.
    #[cfg(unix)]
    #[test]
    fn a_group_spawned_after_cancel_is_killed_when_it_registers() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let dir = std::env::temp_dir().join(format!("p14-late-register-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let started = dir.join("started");
        let side_effect = dir.join("side-effect");
        let request = format!("late-register-{}", std::process::id());

        // The child announces itself, then performs a delayed side effect unless it is stopped.
        let script = format!(
            "printf up > '{}'; sleep 5; printf side > '{}'",
            started.display(),
            side_effect.display()
        );
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command.process_group(0);
        let mut child = command.spawn().unwrap();
        let pgid = child.id() as i32;

        // Barrier: wait until the child is really running, so this models the spawn completing and
        // the cancel arriving in the spawn->register gap.
        let wait_until = Instant::now() + Duration::from_secs(5);
        while !started.exists() && Instant::now() < wait_until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(started.exists(), "the test child never started");

        // The cancel path runs *before* registration: nothing is live, so it drains zero groups.
        assert_eq!(terminate(&request), 0);

        // The late registration must kill the group instead of merely remembering it.
        let guard = register(&request, pgid);
        let mut status = None;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(observed) = child.try_wait().unwrap() {
                status = Some(observed);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if status.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(status.is_some(), "a group spawned after cancel survived its registration");
        assert!(
            status.unwrap().code().is_none(),
            "the late group was not signalled at registration"
        );
        drop(guard);

        // The delayed side effect must never land.
        std::thread::sleep(Duration::from_millis(200));
        assert!(!side_effect.exists(), "a cancelled group produced its delayed side effect");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
