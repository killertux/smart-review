//! GitHub adapter, implemented in M1 (FR-1.1, FR-2.1, FR-2.4, FR-6.3).
//!
//! It will implement `ForgePort` on top of the `gh` CLI. The rules it must obey
//! are already fixed by ARCH-3 and FR-6.5:
//!
//! - always invoke `gh` with `--repo <owner>/<name>`, never relying on the
//!   current directory;
//! - never interpolate user input into a shell string; pass an argv array;
//! - honour `--dry-run` by printing the command instead of running it.
