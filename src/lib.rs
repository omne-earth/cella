//! Library surface for `cella`. `main.rs` is a thin binary wrapper over
//! these modules; splitting them out this way is what lets `tests/*.rs`
//! exercise the virtio/device/freeze logic directly, without a real
//! `/dev/kvm`, as ordinary `cargo test` integration tests.

pub mod boot;
pub mod config;
pub mod devices;
pub mod freeze;
pub mod memory;
pub mod seccomp;
pub mod vcpu;
