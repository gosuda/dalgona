//! Session-scoped execution of the public `JobsOp` service.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dal_core::{JobId, JobsOp, JobsReply, Name};
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

use super::{JobRecord, JobTable};

/// All session-owned inputs needed to execute a job operation.
pub(crate) struct JobsCtx {
    /// The shared table for this session.
    pub(crate) table: Arc<Mutex<JobTable>>,
    /// The current extension; it owns spawned rows and only it may settle them.
    pub(crate) owner: Name,
    /// The session's durable jobs directory.
    pub(crate) jobs_dir: PathBuf,
    /// Stops long polls on session close.
    pub(crate) cancel: CancellationToken,
}

/// Runs one operation against the one session job table.
pub(crate) async fn run_jobs_op(ctx: JobsCtx, op: JobsOp) -> JobsReply {
    match op {
        JobsOp::Spawn {
            name,
            payload: _,
            parent,
        } => spawn(ctx, name, parent).await,
        JobsOp::Status { id } => status(&ctx.table, id).await,
        JobsOp::Find { id } => find(&ctx.table, id).await,
        JobsOp::Cancel { id } => cancel(&ctx, id).await,
        JobsOp::Wait { id, timeout } => wait(&ctx, id, timeout).await,
        JobsOp::List => JobsReply::Listed(ctx.table.lock().await.list()),
        JobsOp::Text { id } => text(&ctx.table, id).await,
        JobsOp::Counts => JobsReply::Counts(ctx.table.lock().await.counts()),
        JobsOp::Ends { after, timeout } => ends(&ctx, after, timeout).await,
        JobsOp::Lines { id, after, timeout } => lines(&ctx, id, after, timeout).await,
        JobsOp::Take { limit } => JobsReply::Taken(ctx.table.lock().await.take(usize::from(limit))),
        JobsOp::Commit { ids } => {
            let committed = ctx.table.lock().await.commit(&ids);
            JobsReply::Committed { ids: committed }
        }
        JobsOp::Release { ids } => {
            let released = ctx.table.lock().await.release(&ids);
            JobsReply::Released { ids: released }
        }
        JobsOp::Hold { id } => set_held(&ctx.table, id, true).await,
        JobsOp::Unhold { id } => set_held(&ctx.table, id, false).await,
        JobsOp::Settle { id, outcome, text } => {
            settle(&ctx.table, &ctx.owner, id, outcome, &text).await
        }
        _ => JobsReply::Unavailable {
            reason: "unknown job operation".into(),
        },
    }
}

async fn spawn(ctx: JobsCtx, name: Name, parent: Option<JobId>) -> JobsReply {
    let id = JobId::new_v7();
    let record = JobRecord::new(
        id,
        name.as_str(),
        ctx.jobs_dir.join(format!("{id:?}.log")),
        CancellationToken::new(),
    )
    .with_parent(parent)
    .owned_by(ctx.owner);
    match ctx.table.lock().await.spawn_owned(record) {
        Ok(()) => JobsReply::Spawned { id },
        Err(error) => JobsReply::Unavailable {
            reason: error.to_string().into(),
        },
    }
}

async fn settle(
    table: &Arc<Mutex<JobTable>>,
    owner: &Name,
    id: JobId,
    outcome: dal_core::JobOutcome,
    text: &str,
) -> JobsReply {
    let mut table = table.lock().await;
    if let Err(error) = table.settle_owned(owner, id, outcome, text) {
        return JobsReply::Refused(error);
    }
    match table.flush().await {
        Ok(()) => JobsReply::Settled { id },
        Err(error) => JobsReply::Unavailable {
            reason: error.to_string().into(),
        },
    }
}

async fn cancel(ctx: &JobsCtx, id: JobId) -> JobsReply {
    let mut table = ctx.table.lock().await;
    if let Err(error) = table.cancel_tree(id) {
        return JobsReply::Refused(error);
    }
    match table.flush().await {
        Ok(()) => JobsReply::Cancelled { id },
        Err(error) => JobsReply::Unavailable {
            reason: error.to_string().into(),
        },
    }
}

async fn status(table: &Arc<Mutex<JobTable>>, id: JobId) -> JobsReply {
    let table = table.lock().await;
    match table.find(id) {
        Some(status) => JobsReply::Status(status),
        None => JobsReply::Refused(dal_core::JobsError::Unknown { id }),
    }
}

async fn find(table: &Arc<Mutex<JobTable>>, id: JobId) -> JobsReply {
    JobsReply::Found(table.lock().await.find(id))
}

async fn text(table: &Arc<Mutex<JobTable>>, id: JobId) -> JobsReply {
    let table = table.lock().await;
    match table.text(id) {
        Some(text) => JobsReply::Text { id, text },
        None => JobsReply::Refused(dal_core::JobsError::Unknown { id }),
    }
}

async fn set_held(table: &Arc<Mutex<JobTable>>, id: JobId, held: bool) -> JobsReply {
    match table.lock().await.set_held(id, held).await {
        Ok(ids) => JobsReply::Held(ids),
        Err(error) => JobsReply::Unavailable {
            reason: error.to_string().into(),
        },
    }
}

async fn wait(ctx: &JobsCtx, id: JobId, timeout: Option<Duration>) -> JobsReply {
    let deadline = timeout.map(|duration| Instant::now() + duration);
    let mut changed = ctx.table.lock().await.subscribe();
    loop {
        {
            let table = ctx.table.lock().await;
            if let Some(outcome) = table.wait(id) {
                return JobsReply::Waited { id, outcome };
            }
            if let Some(status) = table.find(id) {
                if timeout.is_some_and(|duration| duration.is_zero()) {
                    return JobsReply::Status(status);
                }
            } else {
                return JobsReply::Refused(dal_core::JobsError::Unknown { id });
            }
        }
        if let Some(deadline) = deadline {
            tokio::select! {
                biased;
                () = ctx.cancel.cancelled() => return shutdown(),
                () = sleep_until(deadline) => return status(&ctx.table, id).await,
                result = changed.changed() => if result.is_err() { return shutdown(); },
            }
        } else {
            tokio::select! {
                biased;
                () = ctx.cancel.cancelled() => return shutdown(),
                result = changed.changed() => if result.is_err() { return shutdown(); },
            }
        }
    }
}

async fn ends(ctx: &JobsCtx, after: Option<u64>, timeout: Option<Duration>) -> JobsReply {
    let deadline = timeout.map(|duration| Instant::now() + duration);
    let mut changed = ctx.table.lock().await.subscribe();
    loop {
        let read = ctx.table.lock().await.ends_after(after);
        if !read.events.is_empty() || timeout.is_some_and(|duration| duration.is_zero()) {
            return JobsReply::Ended(read);
        }
        if let Some(deadline) = deadline {
            tokio::select! {
                biased;
                () = ctx.cancel.cancelled() => return shutdown(),
                () = sleep_until(deadline) => return JobsReply::Ended(ctx.table.lock().await.ends_after(after)),
                result = changed.changed() => if result.is_err() { return shutdown(); },
            }
        } else {
            tokio::select! {
                biased;
                () = ctx.cancel.cancelled() => return shutdown(),
                result = changed.changed() => if result.is_err() { return shutdown(); },
            }
        }
    }
}

async fn lines(
    ctx: &JobsCtx,
    id: JobId,
    after: Option<u64>,
    timeout: Option<Duration>,
) -> JobsReply {
    let deadline = timeout.map(|duration| Instant::now() + duration);
    let mut changed = ctx.table.lock().await.subscribe();
    loop {
        let read = ctx.table.lock().await.lines_after(id, after);
        let Some(read) = read else {
            return JobsReply::Refused(dal_core::JobsError::Unknown { id });
        };
        if !read.lines.is_empty()
            || read.ended
            || timeout.is_some_and(|duration| duration.is_zero())
        {
            return JobsReply::Lines(read);
        }
        if let Some(deadline) = deadline {
            tokio::select! {
                biased;
                () = ctx.cancel.cancelled() => return shutdown(),
                () = sleep_until(deadline) => {
                    let result = ctx.table.lock().await.lines_after(id, after);
                    return result.map_or_else(
                        || JobsReply::Refused(dal_core::JobsError::Unknown { id }),
                        JobsReply::Lines,
                    );
                },
                result = changed.changed() => if result.is_err() { return shutdown(); },
            }
        } else {
            tokio::select! {
                biased;
                () = ctx.cancel.cancelled() => return shutdown(),
                result = changed.changed() => if result.is_err() { return shutdown(); },
            }
        }
    }
}

fn shutdown() -> JobsReply {
    JobsReply::Unavailable {
        reason: "session shutdown".into(),
    }
}
