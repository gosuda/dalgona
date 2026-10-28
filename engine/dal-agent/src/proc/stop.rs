#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
use tokio::time::sleep;

use std::io;

use process_wrap::tokio::ChildWrapper;

use dal_core::JobOutcome;

use crate::error::ToolError;

#[cfg(target_os = "linux")]
use super::snapshot::ParentGraph;
use super::{ProcStatus, StopReason};

/// Maps a reaped exit status to a terminal process status.
pub(super) fn process_exit_status(status: std::process::ExitStatus) -> ProcStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return ProcStatus::Signaled { signal };
        }
    }
    ProcStatus::Exited {
        code: status.code().unwrap_or(-1),
    }
}

/// Maps an enforced stop reason to its terminal status.
pub(super) fn status_for(reason: StopReason) -> ProcStatus {
    match reason {
        StopReason::Timeout => ProcStatus::TimedOut,
        StopReason::Cancelled => ProcStatus::Cancelled,
    }
}

/// Maps a terminal process status to its durable job outcome.
pub(super) fn outcome_for(status: ProcStatus) -> JobOutcome {
    match status {
        ProcStatus::Exited { code } => JobOutcome::Exited { code },
        ProcStatus::Signaled { signal } => JobOutcome::Failed {
            message: format!("Command was killed by signal {signal}").into_boxed_str(),
        },
        ProcStatus::TimedOut => JobOutcome::Failed {
            message: "Command timed out".into(),
        },
        ProcStatus::Cancelled => JobOutcome::Cancelled,
    }
}

/// Wraps a process-wait failure without erasing its kind.
pub(super) fn process_failure(error: &io::Error) -> ToolError {
    ToolError::Failed(Box::new(io::Error::new(
        error.kind(),
        format!("process wait failed: {error}"),
    )))
}

/// Sends the soft stop to the whole group or job object.
pub(super) fn soft_kill(child: &mut Box<dyn ChildWrapper>) -> Result<(), ToolError> {
    #[cfg(unix)]
    {
        child.signal(15).map_err(|error| process_failure(&error))?;
        Ok(())
    }
    #[cfg(windows)]
    {
        child.start_kill().map_err(|error| process_failure(&error))
    }
}

/// Sends the hard stop to the whole group or job object.
pub(super) fn hard_kill(child: &mut Box<dyn ChildWrapper>) -> Result<(), ToolError> {
    #[cfg(unix)]
    {
        child.signal(9).map_err(|error| process_failure(&error))?;
        Ok(())
    }
    #[cfg(windows)]
    {
        child.start_kill().map_err(|error| process_failure(&error))
    }
}

/// Sweeps the process group once more after the leader exits.
pub(super) fn sweep_process_group(leader_pid: u32) {
    #[cfg(unix)]
    {
        use rustix::process::{Pid, Signal, kill_process_group};
        let Ok(raw) = i32::try_from(leader_pid) else {
            return;
        };
        let Some(pid) = Pid::from_raw(raw) else {
            return;
        };
        let _ = kill_process_group(pid, Signal::KILL);
    }
    #[cfg(windows)]
    {
        let _ = leader_pid;
    }
}

/// Builds one global parent-to-children map from a full `/proc` census.
///
/// Reading every pid's `stat` is the expensive part of a descendant scan;
/// batching several simultaneous stop requests reuses one walk for a
/// whole admission batch while each caller still receives only its own
/// reachable subtree. A census that cannot be opened is a typed error:
/// the pre-signal stop path must never read a lost census as an empty
/// descendant set. Individual `stat` reads keep their skip semantics; a
/// pid that vanishes mid-walk is simply absent.
#[cfg(target_os = "linux")]
pub(super) fn proc_parent_graph() -> Result<ParentGraph, ToolError> {
    use std::collections::HashMap;

    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let entries = std::fs::read_dir("/proc").map_err(|source| {
        ToolError::Failed(Box::new(io::Error::new(
            source.kind(),
            format!("the /proc census could not be opened: {source}"),
        )))
    })?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let stat_path = format!("/proc/{pid}/stat");
        let Ok(stat) = std::fs::read_to_string(stat_path) else {
            continue;
        };
        let Some(comm_end) = stat.rfind(')') else {
            continue;
        };
        let rest = stat[comm_end + 1..].trim_start();
        let mut parts = rest.split_whitespace();
        let _state = parts.next();
        let Some(ppid_text) = parts.next() else {
            continue;
        };
        let Ok(parent_pid) = ppid_text.parse::<u32>() else {
            continue;
        };
        children.entry(parent_pid).or_default().push(pid);
    }
    Ok(children)
}

/// Walks `graph` for the live descendants of `leader_pid`.
///
/// Non-consuming, so one shared graph serves a whole batch; the visited
/// set matches the previous single-caller BFS with the leader excluded,
/// unordered like the previous `HashSet` collect.
#[cfg(target_os = "linux")]
pub(super) fn graph_reach(graph: &ParentGraph, leader_pid: u32) -> Vec<u32> {
    use std::collections::{HashSet, VecDeque};

    let mut doomed = HashSet::new();
    let mut pending = VecDeque::from([leader_pid]);
    while let Some(parent_pid) = pending.pop_front() {
        let Some(descendants) = graph.get(&parent_pid) else {
            continue;
        };
        for pid in descendants {
            if doomed.insert(*pid) {
                pending.push_back(*pid);
            }
        }
    }
    doomed.into_iter().collect()
}

/// Walks Linux `/proc` for the live descendants of a leader pid.
///
/// The ppid chain is only intact while the lineage still lives: a detached
/// (`setsid`) descendant reparents to init the instant its bridge dies, so
/// callers must snapshot before signaling if they need the detached set.
/// Best-effort legacy shape for the post-exit sweeps: a lost census reads
/// as no descendants, exactly as before the split. The pre-signal stop
/// path never comes through here; it consumes the typed census error.
#[cfg(target_os = "linux")]
pub(super) fn proc_descendants(leader_pid: u32) -> Vec<u32> {
    let graph = proc_parent_graph().unwrap_or_default();
    graph_reach(&graph, leader_pid)
}

/// Sweeps Linux `/proc` for descendants of recorded leader pids.
#[cfg(target_os = "linux")]
pub(super) fn sweep_proc_descendants(leader_pid: u32) {
    sweep_recorded(&proc_descendants(leader_pid));
}

/// Kills every pid a prior `proc_descendants` snapshot recorded.
///
/// `setsid` escapees reparent to init when their bridge dies, which severs
/// the lineage the post-exit `/proc` sweep relies on; the snapshot taken
/// before signaling is the only place they are still reachable. A recorded
/// pid recycled between snapshot and sweep is an accepted narrow race.
pub(super) fn sweep_recorded(pids: &[u32]) {
    #[cfg(unix)]
    {
        use rustix::process::{Pid, Signal, kill_process};

        for pid in pids {
            let Ok(raw) = i32::try_from(*pid) else {
                continue;
            };
            let Some(target) = Pid::from_raw(raw) else {
                continue;
            };
            let _ = kill_process(target, Signal::KILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = pids;
    }
}

/// Waits for the leader without blocking on surviving job descendants.
///
/// `JobObjectChild::wait` blocks until the whole job empties, so a detached
/// grandchild holding a pipe would stall the normal-exit path before any
/// sweep runs. Windows polls the outer wrapper because its job-object
/// `try_wait` semantics are platform-specific; Unix call sites instead use
/// `inner_mut().try_wait()` to observe only the native leader before signaling
/// the outer process-group wrapper and retaining its later `wait()` for reaping.
#[cfg(windows)]
pub(super) async fn wait_leader_exit(
    child: &mut Box<dyn ChildWrapper>,
) -> Result<std::process::ExitStatus, ToolError> {
    loop {
        if let Some(status) = child.try_wait().map_err(|error| process_failure(&error))? {
            let _ = child.start_kill();
            return Ok(status);
        }
        sleep(Duration::from_millis(10)).await;
    }
}
