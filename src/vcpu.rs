//! vCPU creation, run loop, and the register state that freeze/thaw
//! captures.
//!
//! Single vCPU only (see freeze.rs for why that matters for TSC
//! handling). No irqfd/ioeventfd: PIO and MMIO exits are trapped and
//! handled synchronously in this thread, and devices call
//! `VmFd::set_irq_line` directly to raise legacy IRQs through the
//! in-kernel PIC/IOAPIC.

use kvm_bindings::{
    kvm_clock_data, kvm_fpu, kvm_lapic_state, kvm_mp_state, kvm_msr_entry, kvm_regs, kvm_sregs,
    kvm_vcpu_events, CpuId, Msrs, KVM_MAX_CPUID_ENTRIES, KVM_MP_STATE_RUNNABLE,
};
use kvm_ioctls::{Kvm, VcpuExit, VcpuFd, VmFd};

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
];

#[allow(dead_code)] // fields read via {:?} in error messages, not field access
#[derive(Debug)]
pub enum Error {
    Kvm(kvm_ioctls::Error),
    MsrCountMismatch,
}
impl From<kvm_ioctls::Error> for Error {
    fn from(e: kvm_ioctls::Error) -> Self {
        Error::Kvm(e)
    }
}

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
    pub msrs: [(u32, u64); 16],
}

/// Filtered CPUID: whatever this host's KVM supports, capped to the
/// bindings' max entry count.
pub fn supported_cpuid(kvm: &Kvm) -> Result<CpuId, Error> {
    Ok(kvm.get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)?)
}

pub fn create_vcpu(vm: &VmFd, cpuid: &CpuId) -> Result<VcpuFd, Error> {
    let vcpu = vm.create_vcpu(0)?;
    vcpu.set_cpuid2(cpuid)?;
    let mut mp = vcpu.get_mp_state()?;
    mp.mp_state = KVM_MP_STATE_RUNNABLE;
    vcpu.set_mp_state(mp)?;
    Ok(vcpu)
}

pub fn save(vcpu: &VcpuFd) -> Result<VcpuState, Error> {
    let mut msrs_req = Msrs::from_entries(
        &SAVED_MSRS
            .iter()
            .map(|&index| kvm_msr_entry {
                index,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )
    .expect("static MSR list is well-formed");
    vcpu.get_msrs(&mut msrs_req)?;

    let mut msrs = [(0u32, 0u64); 16];
    for (i, e) in msrs_req.as_slice().iter().enumerate() {
        if i >= msrs.len() {
            break;
        }
        msrs[i] = (e.index, e.data);
    }

    Ok(VcpuState {
        regs: vcpu.get_regs()?,
        sregs: vcpu.get_sregs()?,
        fpu: vcpu.get_fpu()?,
        mp_state: vcpu.get_mp_state()?,
        lapic: vcpu.get_lapic()?,
        events: vcpu.get_vcpu_events()?,
        msrs,
    })
}

/// Restore everything captured by `save`. Order matters: sregs/regs
/// before MSRs, and the TSC MSR (which re-arms the counter to its frozen
/// value) written last so nothing else we set perturbs it. This direct
/// MSR-write approach to TSC restore is only correct because we are
/// single-vCPU: KVM's TSC synchronization heuristics exist to keep
/// *multiple* vCPUs' counters aligned, which doesn't apply here.
pub fn restore(vcpu: &VcpuFd, state: &VcpuState) -> Result<(), Error> {
    vcpu.set_sregs(&state.sregs)?;
    vcpu.set_regs(&state.regs)?;
    vcpu.set_fpu(&state.fpu)?;
    vcpu.set_mp_state(state.mp_state)?;
    vcpu.set_lapic(&state.lapic)?;
    vcpu.set_vcpu_events(&state.events)?;

    let entries: Vec<kvm_msr_entry> = state
        .msrs
        .iter()
        .filter(|(index, _)| *index != 0 || state.msrs[0].0 == 0)
        .map(|(index, data)| kvm_msr_entry {
            index: *index,
            data: *data,
            ..Default::default()
        })
        .collect();
    let msrs_req = Msrs::from_entries(&entries).map_err(|_| Error::MsrCountMismatch)?;
    vcpu.set_msrs(&msrs_req)?;
    Ok(())
}

pub fn save_vm_clock(vm: &VmFd) -> Result<kvm_clock_data, Error> {
    Ok(vm.get_clock()?)
}

pub fn restore_vm_clock(vm: &VmFd, clock: &kvm_clock_data) -> Result<(), Error> {
    // Deliberately not the REALTIME flag: we want the guest's kvmclock to
    // resume counting from where it stopped (monotonic-continuous), not
    // to jump to the host's current wall time. See freeze.rs.
    let mut c = *clock;
    c.flags = 0;
    vm.set_clock(&c)?;
    Ok(())
}

/// Outcome of one KVM_RUN dispatch, so the caller (main.rs) knows whether
/// to keep looping.
pub enum RunResult {
    Continue,
    Halted,
    Shutdown,
}

/// Handle exactly one `VcpuExit`. PIO in the 0x3f8-0x3ff range goes to the
/// serial device; MMIO in a device's configured window goes to its
/// transport. Everything else either no-ops (Intr, IrqWindowOpen -- both
/// expected wakeups with nothing to do) or is treated as fatal.
pub struct Devices<'a> {
    pub serial: &'a mut crate::devices::serial::SerialDevice,
    pub mmio_devices: &'a mut [(u64, u64, crate::devices::virtio::mmio::MmioTransport)], // (base, len, transport)
    pub mem: &'a vm_memory::GuestMemoryMmap,
}

pub fn dispatch(exit: VcpuExit, devices: &mut Devices) -> RunResult {
    match exit {
        VcpuExit::IoIn(port, data) => {
            if (0x3f8..0x400).contains(&port) {
                data[0] = devices.serial.read(port);
            } else {
                data.fill(0xff);
            }
            RunResult::Continue
        }
        VcpuExit::IoOut(port, data) => {
            if (0x3f8..0x400).contains(&port) && !data.is_empty() {
                devices.serial.write(port, data[0]);
            }
            RunResult::Continue
        }
        VcpuExit::MmioRead(addr, data) => {
            if let Some((base, _, t)) = devices
                .mmio_devices
                .iter_mut()
                .find(|(base, len, _)| addr >= *base && addr < base + len)
            {
                t.read(addr - *base, data);
            } else {
                data.fill(0);
            }
            RunResult::Continue
        }
        VcpuExit::MmioWrite(addr, data) => {
            if let Some((base, _, t)) = devices
                .mmio_devices
                .iter_mut()
                .find(|(base, len, _)| addr >= *base && addr < base + len)
            {
                let base = *base;
                t.write(addr - base, data, devices.mem);
            }
            RunResult::Continue
        }
        VcpuExit::Hlt => RunResult::Halted,
        VcpuExit::Shutdown | VcpuExit::FailEntry(..) => RunResult::Shutdown,
        VcpuExit::Intr | VcpuExit::IrqWindowOpen => RunResult::Continue,
        _ => RunResult::Continue,
    }
}
