//! Owned-file machinery for the MYCode home.
//!
//! The owned-file layer adds bounded reads, persistent locks, and
//! handle-relative atomic replacement without defining any document schema.
//! Authority documents consume it publicly.

pub(crate) mod owned_file;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
