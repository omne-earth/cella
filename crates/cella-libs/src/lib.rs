//! cella-libs: the commons, by observation (see tasks/PHASE1-core.md
//! 1.6.13). Every module here has two or more genuine users; the
//! features are the thinness proof -- a persona that does not
//! enable a feature does not compile its code.

pub mod config;

#[cfg(feature = "golden")]
pub mod golden;

#[cfg(feature = "machine")]
pub mod seq;

#[cfg(feature = "wire")]
pub mod ledger;
#[cfg(feature = "wire")]
pub mod proto;

#[cfg(feature = "sidecar")]
pub mod freeze;
#[cfg(feature = "sidecar")]
pub mod sidecar;

#[cfg(feature = "audit")]
pub mod audit;

#[cfg(feature = "machine")]
pub mod machine;

#[cfg(feature = "jail")]
pub mod jail;

#[cfg(feature = "seccomp")]
pub mod seccomp;
