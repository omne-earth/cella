//! The VMM's own modules, exposed as a library for the integration
//! tests alone (tests/ drives the virtio logic with no KVM). The
//! binary is the product; nothing else links this.

pub mod boot;
pub mod devices;
pub mod memory;
pub mod seccomp;
pub mod vcpu;
pub mod warm;
