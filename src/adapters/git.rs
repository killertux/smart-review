//! Git adapter, implemented in M2 (FR-3.1, FR-3.2).
//!
//! It will implement `WorkspacePort`: fetch a pull request head into a managed
//! worktree, resolve the merge base, and produce diffs and file contents for a
//! given revision. The user's working tree, index, HEAD and branches are never
//! modified (DEC-1).
