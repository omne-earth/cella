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
// ipv6.disable=1: the guest needs no IPv6, and under the total
// membrane its router solicitations and MLD reports would park and
// freeze the machine on chatter no one sent (docs/NETWORK-MODEL.md).
// Chatter that exists still parks; this removes the pointless source.
pub const DEFAULT_BASE_ARGS: &str = "console=ttyS0 reboot=k panic=1 pci=off ipv6.disable=1";

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
/// How a thaw warms the stage-2 mappings before the clock restore.
/// "deep": KVM_PRE_FAULT_MEMORY for the direct host, then the warming
/// stub of src/warm.rs, which reaches every layer below through real
/// guest accesses. "ept": the ioctl only. "off": the cold thaw, for a
/// measurement. Measured without warming: ~25 ms of guest-visible
/// lateness on nested KVM; with "ept": ~4 ms remains for each nesting
/// level below the direct host; "deep" exists to remove that
/// remainder. The cost of the warming falls outside the clock window
/// of the guest. Override with CELLA_THAW_PREFAULT.
pub const DEFAULT_THAW_PREFAULT: &str = "deep";

/// The static addresses of the TAP subnet, in one place: the guest
/// address and the host gateway. `sudo cella setup net` creates the
/// host side; a networked machine uses these on its ip= argument.
/// The sub-id range the install scripts delegate and every
/// operator-facing message suggests (1.6.14a). A hint, not the
/// mechanism: machine_identity reads the range actually delegated
/// in /etc/subuid and /etc/subgid at run time, whatever it is.
/// One source, so the suggestions cannot diverge; 524288 sits
/// above the ranges systemd-homed and rootless container tools
/// typically claim.
pub const SUBID_RANGE_HINT: &str = "524288-589823";

pub const DEFAULT_GUEST_IP: &str = "192.168.200.2";
pub const DEFAULT_HOST_IP: &str = "192.168.200.1";

pub fn default_cmdline() -> String {
    format!("{DEFAULT_BASE_ARGS} {DEFAULT_TIME_ARGS}")
}
