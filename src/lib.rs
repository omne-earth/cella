//! Library surface for `cella`. `main.rs` is a thin binary wrapper over
//! these modules; splitting them out this way is what lets `tests/*.rs`
//! exercise the virtio/device/freeze logic directly, without a real
//! `/dev/kvm`, as ordinary `cargo test` integration tests.

pub mod boot;
pub mod build;
pub mod config;
pub mod devices;
pub mod doctor;
pub mod freeze;
pub mod golden;
pub mod machine;
pub mod memory;
pub mod proto;
pub mod seccomp;
pub mod universe;
pub mod vcpu;
pub mod warm;
