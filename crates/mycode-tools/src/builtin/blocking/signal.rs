//! SIGURG signal ownership for Unix worker interruption.

use std::io;
use std::sync::Mutex;

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct SignalGuard;

#[cfg(unix)]
pub(super) struct SignalState {
    refs: usize,
    previous: libc::sigaction,
}

#[cfg(unix)]
pub(super) fn signal_state() -> &'static Mutex<Option<SignalState>> {
    static STATE: Mutex<Option<SignalState>> = Mutex::new(None);
    &STATE
}

#[cfg(unix)]
pub(super) fn our_sigurg_handler() -> usize {
    interrupt_signal_handler as *const () as usize
}

#[cfg(unix)]
pub(super) fn current_sigurg_handler() -> io::Result<usize> {
    // SAFETY: querying the current action with a null new-action pointer.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGURG, std::ptr::null(), &mut current) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(current.sa_sigaction)
    }
}

#[cfg(unix)]
pub(super) fn handler_is_default(handler: usize) -> bool {
    handler == libc::SIG_DFL
}

/// Installs an owned `SIGURG` handler, or reuses the one this crate owns.
///
/// Fails closed when another component already owns `SIGURG`. The last guard
/// restores the previous disposition only while the current handler is still
/// this crate's. Cancellation uses [`CancellationToken`]; a per-worker socket
/// wakes pollable reads, while `SIGURG` is a best-effort wake for other Unix
/// syscalls only while this crate still owns the disposition.
///
/// # Errors
///
/// Returns an I/O error when `sigaction` fails or a foreign handler is
/// installed.
#[cfg(unix)]
pub(crate) fn acquire_interrupt_signal() -> io::Result<SignalGuard> {
    let mut slot = signal_state()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    match slot.as_mut() {
        Some(state) => {
            let current = current_sigurg_handler()?;
            if current != our_sigurg_handler() {
                return Err(io::Error::other(
                    "SIGURG handler was replaced; search interrupt cannot be established",
                ));
            }
            state.refs = state.refs.saturating_add(1);
            Ok(SignalGuard)
        }
        None => {
            let current = current_sigurg_handler()?;
            if current != our_sigurg_handler() && !handler_is_default(current) {
                return Err(io::Error::other(
                    "SIGURG is owned by another handler; search interrupt cannot be established",
                ));
            }
            let previous = install_our_sigurg()?;
            *slot = Some(SignalState { refs: 1, previous });
            Ok(SignalGuard)
        }
    }
}

#[cfg(unix)]
pub(super) fn install_our_sigurg() -> io::Result<libc::sigaction> {
    // SAFETY: the action has the documented layout, uses an empty handler,
    // and omits SA_RESTART so restartable blocking calls return EINTR.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = our_sigurg_handler();
        if libc::sigemptyset(&mut action.sa_mask) != 0 {
            return Err(io::Error::last_os_error());
        }
        action.sa_flags = 0;
        let mut previous: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGURG, &action, &mut previous) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(previous)
    }
}

#[cfg(unix)]
impl Drop for SignalGuard {
    fn drop(&mut self) {
        let mut slot = signal_state()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(state) = slot.as_mut() else {
            return;
        };
        state.refs = state.refs.saturating_sub(1);
        if state.refs != 0 {
            return;
        }
        let previous = state.previous;
        *slot = None;
        let Ok(current) = current_sigurg_handler() else {
            return;
        };
        if current != our_sigurg_handler() {
            return;
        }
        // SAFETY: restore only while the current handler is still ours.
        unsafe {
            libc::sigaction(libc::SIGURG, &previous, std::ptr::null_mut());
        }
    }
}

#[cfg(unix)]
pub(super) fn unblock_interrupt_signal() -> io::Result<()> {
    // SAFETY: `set` is initialized by `sigemptyset`/`sigaddset`; passing null
    // for the old set is documented. pthread APIs return an errno value.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&mut set) != 0 || libc::sigaddset(&mut set, libc::SIGURG) != 0 {
            return Err(io::Error::last_os_error());
        }
        let status = libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status));
        }
    }
    Ok(())
}

#[cfg(unix)]
extern "C" fn interrupt_signal_handler(_signal: libc::c_int) {}
