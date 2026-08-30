//! The default guest configuration, in one place.
//!
//! These values were in six files: main.rs, the three smoke tests, and
//! the two probes. A change had to be made six times, and a missed file
//! gave one workflow a different guest from the others.
//!
//! The shell scripts read these values from the binary, with
//! `cella --print-default-cmdline`. The probes use the constants
//! directly.

/// The part of the kernel command line that is not about time or about
/// devices. The console is ttyS0 because the 8250 serial device is the
/// only channel out of the guest. `pci=off` is correct because cella
/// emulates no PCI bus.
pub const DEFAULT_BASE_ARGS: &str = "console=ttyS0 reboot=k panic=1 pci=off";

/// The kernel arguments that control time in the guest.
///
/// `clocksource=kvm-clock` makes kvm-clock the single source of time.
/// cella restores kvm-clock exactly at a thaw, and KVM re-establishes
/// the anchor of the pvclock page.
///
/// `tsc=reliable` stops the clocksource watchdog. Without it, the
/// watchdog compares the TSC against kvm-clock after a thaw, measures a
/// difference of 5 ms to 27 ms, and marks the TSC unstable. The watchdog
/// exists to find faults in hardware. Here the difference comes from an
/// act of cella: cella rewinds the TSC at every thaw (see vcpu.rs).
///
/// Note the limit of `tsc=reliable`. It tells the guest that the TSC is
/// a reliable timeline, and that is not true across a thaw. The guest
/// does not act on the claim, because `clocksource=kvm-clock` keeps
/// kvm-clock selected and the guest does not read the TSC for
/// timekeeping. `tsc=nowatchdog` gives the same result and makes no
/// claim about the TSC. See the CELLA_TIME_ARGS comment in the Makefile
/// for the measurement of each value.
///
/// `trace_clock=local` keeps the per-CPU clock for the ftrace ring
/// buffer. Without it the guest prints "Unstable clock detected,
/// switching default tracing clock" at boot and moves ftrace to the
/// "global" clock, which is slower. The guest has one vCPU, therefore
/// the "global" clock gives no benefit: it exists to make timestamps
/// comparable between CPUs. This argument affects the tracing subsystem
/// only, and not timekeeping.
pub const DEFAULT_TIME_ARGS: &str = "tsc=reliable clocksource=kvm-clock trace_clock=local";

/// The complete default command line, without the arguments for the root
/// filesystem and the virtio devices. A caller that gives `--cmdline`
/// replaces this value.
/// Fill the stage-2 page tables of a thawed VM before the clock restore
/// (KVM_PRE_FAULT_MEMORY, Linux 6.11+). Without the prefill, the first
/// heartbeat cycle of the guest pays one stage-2 fault for each page
/// that it touches, and the clock of the guest counts that time
/// (measured on nested KVM: ~25 ms; ~4 ms with the prefill). The
/// cost of the prefill falls outside the clock window of the guest.
/// Set CELLA_THAW_PREFAULT=off to measure the cold thaw.
pub const DEFAULT_THAW_PREFAULT: bool = true;

pub fn default_cmdline() -> String {
    format!("{DEFAULT_BASE_ARGS} {DEFAULT_TIME_ARGS}")
}
