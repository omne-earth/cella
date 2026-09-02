//! The sidecar's state types: what the freeze writes and the thaw
//! reads. Two genuine users put them here (the observation rule of
//! 1.6.13): the VMM writes the sidecar, and the gateway's inspect
//! reads it.

use kvm_bindings::{
    kvm_fpu, kvm_irqchip, kvm_lapic_state, kvm_mp_state, kvm_pit_state2, kvm_regs, kvm_sregs,
    kvm_vcpu_events, kvm_xcrs,
};

/// MSRs we save/restore across freeze/thaw. Deliberately not exhaustive
/// (no perf-counter or vendor-specific MSRs): this is the set a plain
/// 64-bit Linux guest actually programs before we might freeze it.
pub const SAVED_MSRS: &[u32] = &[
    0x0000_0010, // MSR_IA32_TSC
    0xc000_0080, // MSR_EFER
    0x0000_001b, // MSR_IA32_APICBASE
    0x0000_0174, // MSR_IA32_SYSENTER_CS
    0x0000_0175, // MSR_IA32_SYSENTER_ESP
    0x0000_0176, // MSR_IA32_SYSENTER_EIP
    0xc000_0081, // MSR_STAR
    0xc000_0082, // MSR_LSTAR
    0xc000_0083, // MSR_CSTAR
    0xc000_0084, // MSR_SYSCALL_MASK
    0xc000_0102, // MSR_KERNEL_GS_BASE
    0x4b56_4d00, // MSR_KVM_WALL_CLOCK_NEW
    0x4b56_4d01, // MSR_KVM_SYSTEM_TIME_NEW
    0x4b56_4d02, // MSR_KVM_ASYNC_PF_EN
    0x4b56_4d03, // MSR_KVM_STEAL_TIME
    0x4b56_4d04, // MSR_KVM_PV_EOI_EN
    // The supervisor xstate mask. XRSTORS takes a #GP fault when the
    // XCOMP_BV of an xsave area holds a component that XCR0 | IA32_XSS
    // does not enable. On a CPU with supervisor xstates (CET and others)
    // the guest kernel sets XSS bits at boot, and the fpstate of every
    // task holds those bits in XCOMP_BV. A thaw without this MSR gives
    // XSS = 0, and the guest then panics in restore_fpregs_from_fpstate
    // at its first context switch. Host-initiated reads and writes of
    // this MSR are always permitted, thus this entry is safe on a CPU
    // without supervisor xstates (the value is then 0).
    0x0000_0da0, // MSR_IA32_XSS
    // The deadline of the TSC-deadline LAPIC timer. KVM_SET_LAPIC starts
    // the timer from apic->lapic_timer.tscdeadline. Only a WRMSR writes
    // that field. Therefore, if you do not restore this MSR, the thaw
    // starts no timer. A guest that waits in HLT for the next timer
    // interrupt then does not continue. This MSR comes after
    // MSR_IA32_TSC in the list. The deadline is a TSC value, thus the TSC
    // must be correct before you write the deadline.
    0x0000_06e0, // MSR_IA32_TSC_DEADLINE
];

/// The number of entries in `VcpuState::msrs`. This value comes from
/// SAVED_MSRS. Therefore the list and the array cannot become different.
/// An earlier version used a constant length of 16. If you added an MSR
/// to the list, `save` did not store the last entry and gave no error.
pub const SAVED_MSR_COUNT: usize = SAVED_MSRS.len();

/// Everything about a vCPU that isn't guest RAM. Copy types throughout
/// (see freeze.rs for how this gets written to the sidecar file).
#[derive(Clone, Copy)]
pub struct VcpuState {
    pub regs: kvm_regs,
    pub sregs: kvm_sregs,
    pub fpu: kvm_fpu,
    pub mp_state: kvm_mp_state,
    pub lapic: kvm_lapic_state,
    pub events: kvm_vcpu_events,
    pub msrs: [(u32, u64); SAVED_MSR_COUNT],
    /// The extended FPU state (the AVX and AVX-512 registers) and XCR0.
    /// The `kvm_fpu` field above contains only the legacy x87 and SSE
    /// area. Without these two fields, a thawed guest gets an XCR0 value
    /// for x87 only. The first AVX instruction then causes an
    /// invalid-opcode fault. The RNG in the guest executes such an
    /// instruction, in blake2s.
    pub xsave_region: [u32; 1024],
    pub xcrs: kvm_xcrs,
}

/// The interrupt hardware that KVM keeps, and that guest RAM does not
/// contain: the two legacy PICs, the IOAPIC, and the PIT.
///
/// This state is not part of a vCPU, and the process does not keep it
/// after it exits. A thaw makes a new irqchip and a new PIT (see
/// main.rs). This operation clears the IOAPIC routing that the guest set,
/// and it clears the PIT channel programming. A guest that you freeze
/// when it is idle then waits for a timer interrupt that does not occur,
/// because the device that must send the interrupt no longer has the
/// necessary configuration.
#[derive(Clone, Copy)]
pub struct IrqChipState {
    pub pic_master: kvm_irqchip,
    pub pic_slave: kvm_irqchip,
    pub ioapic: kvm_irqchip,
    pub pit: kvm_pit_state2,
}

/// The device-side position of one queue. The rings live in guest RAM,
/// which the freeze preserves; these fields are the private view of the
/// device, and the sidecar must carry them (see docs/DEVICE-STATE.md).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct QueueState {
    pub ready: bool,
    pub size: u16,
    pub desc_table: u64,
    pub avail_ring: u64,
    pub used_ring: u64,
    pub next_avail: u16,
    pub next_used: u16,
}

/// Everything a thaw must put back into a fresh MmioTransport. The
/// held egress frames are frames read from the TX ring and not yet
/// written to the TAP at the freeze instant (see docs/DEVICE-STATE.md).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TransportState {
    pub status: u32,
    pub queue_sel: u32,
    pub isr: u32,
    pub driver_features: u64,
    pub queues: Vec<QueueState>,
    /// Parked egress frames: the descriptor head index, and the frame
    /// bytes (vnet header included).
    pub held_frames: Vec<(u16, Vec<u8>)>,
    /// The inbound lane's held frames (undecided), and the judged
    /// frames still awaiting free RX descriptors. Frames alone: at
    /// thaw the ids rebind through the chronicle by the never-guess
    /// matcher, like the egress lane's.
    pub ingress_held: Vec<Vec<u8>>,
    pub ingress_deliverable: Vec<Vec<u8>>,
}
