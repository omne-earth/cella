//! The build orchestrator: golden kernels and root filesystems,
//! natively, through the toolbox. Its one other user is the
//! doctor's fix (see tasks/PHASE1-core.md 1.6.13).

pub mod flags;
pub mod orchestrate;
pub mod seccomp;
