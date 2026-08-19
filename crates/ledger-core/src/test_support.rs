//! Shared test-support helpers. NOT part of the ledger API — this
//! module exists so downstream crates' test suites can reuse it
//! (`#[cfg(test)]` items are invisible across crate boundaries).

/// True when the current process runs with root privileges.
///
/// chmod-based failure injection is inert as root (root bypasses
/// permission checks), so tests that rely on it must skip instead of
/// failing spuriously in root-run CI containers. Asks the kernel
/// directly rather than shelling out to `id`, so a missing binary
/// can't silently report "not root".
#[cfg(unix)]
pub fn running_as_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}
