//! Process-lifecycle classification shared by the subprocess families.

use std::io;

/// `ESRCH`: no process or process group matches the signal target.
#[cfg(unix)]
const ESRCH: i32 = 3;

/// Whether a termination or reaping error means the child or its group no longer exists.
///
/// `NotFound` covers a missing child. `killpg` on a group whose last member was reaped reports
/// `ESRCH`, which `std` leaves uncategorized, so it never maps to `NotFound`. Both mean there is
/// nothing left to signal or reap; every other error is a real cleanup failure.
pub(super) fn is_process_gone(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::NotFound {
        return true;
    }
    #[cfg(unix)]
    if error.raw_os_error() == Some(ESRCH) {
        return true;
    }
    false
}
