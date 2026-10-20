//! Windows file-sharing semantics, kept in one place so Unix behaviour stays exactly as it is.
//!
//! Two Windows behaviours make cache operations fail where Unix succeeds:
//!
//! - **Delete pending.** A file deleted while another process still has it open or mapped can
//!   linger under its name until the last handle closes (when the volume or the delete does not
//!   use POSIX semantics). Pack segments stay mapped (memmap2 keeps a duplicated handle) for as
//!   long as a reader's `Cache` lives, so a segment gc deletes can stay "delete pending" for the
//!   rest of that reader's life. Opening it fails with `ERROR_ACCESS_DENIED` (the NT status is
//!   `STATUS_DELETE_PENDING`), not `NotFound`. For a reader the file is gone: [`is_gone`].
//! - **Sharing violations.** A process holding a file without the sharing we ask for (virus
//!   scanners and indexers open fresh files) makes opens and renames fail for a moment with
//!   `ERROR_SHARING_VIOLATION`. [`retry_busy`] retries those briefly.
//!
//! On Unix none of these codes are special: `is_gone` is `NotFound` and `retry_busy` runs once.

use std::io;
#[cfg(windows)]
use std::time::Duration;

/// Retries after the first attempt; delays double from 1 ms (≈ 31 ms in total).
#[cfg(windows)]
const BUSY_RETRIES: u32 = 5;

/// The file is missing, or (Windows) being deleted by another process that still has it open.
pub(crate) fn is_gone(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound || is_delete_pending(e)
}

/// Windows: opening or replacing a file that is pending deletion. `ERROR_ACCESS_DENIED` is also
/// what a real permission problem returns; inside the cache directory, which this crate created,
/// a file being deleted is by far the likelier cause, and either way the read is a miss.
#[cfg(windows)]
pub(crate) fn is_delete_pending(e: &io::Error) -> bool {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_DELETE_PENDING: i32 = 303;
    matches!(
        e.raw_os_error(),
        Some(ERROR_ACCESS_DENIED | ERROR_DELETE_PENDING)
    )
}

#[cfg(not(windows))]
pub(crate) fn is_delete_pending(_: &io::Error) -> bool {
    false
}

/// Windows: another handle denies the sharing (or holds a byte-range lock) we need. Transient.
#[cfg(windows)]
fn is_sharing_violation(e: &io::Error) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    matches!(
        e.raw_os_error(),
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
    )
}

/// Runs `op`, retrying it a bounded number of times while it fails with a sharing violation
/// (Windows only; elsewhere it runs exactly once).
pub(crate) fn retry_busy<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    #[cfg(windows)]
    {
        let mut delay = Duration::from_millis(1);
        for _ in 0..BUSY_RETRIES {
            match op() {
                Err(e) if is_sharing_violation(&e) => {
                    std::thread::sleep(delay);
                    delay *= 2;
                }
                other => return other,
            }
        }
    }
    op()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_is_windows_only() {
        let denied = io::Error::from_raw_os_error(5);
        assert_eq!(is_gone(&denied), cfg!(windows));
        assert!(is_gone(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(!is_gone(&io::Error::from(io::ErrorKind::InvalidData)));
    }

    #[test]
    fn retries_only_sharing_violations_and_is_bounded() {
        let mut calls = 0;
        let r: io::Result<()> = retry_busy(|| {
            calls += 1;
            Err(io::Error::from_raw_os_error(32))
        });
        assert!(r.is_err());
        assert_eq!(calls, if cfg!(windows) { 6 } else { 1 });

        let mut calls = 0;
        let r = retry_busy(|| {
            calls += 1;
            if calls < 3 {
                Err(io::Error::from_raw_os_error(32))
            } else {
                Ok(calls)
            }
        });
        if cfg!(windows) {
            assert_eq!(r.unwrap(), 3);
        } else {
            assert!(r.is_err());
        }

        let mut calls = 0;
        let r: io::Result<()> = retry_busy(|| {
            calls += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert!(r.is_err());
        assert_eq!(calls, 1);
    }
}
