//! vCPU creation, run loop, and the register state that freeze/thaw
//! captures.
//!
//! Single vCPU only (see freeze.rs for why that matters for TSC
//! handling). No irqfd/ioeventfd: PIO and MMIO exits are trapped and
//! handled synchronously in this thread, and devices call
//! `VmFd::set_irq_line` directly to raise legacy IRQs through the
//! in-kernel PIC/IOAPIC.

use kvm_bindings::{
    kvm_clock_data, kvm_irqchip, kvm_msr_entry, kvm_xsave, CpuId, Msrs, KVM_MAX_CPUID_ENTRIES,
    KVM_MP_STATE_RUNNABLE,
};
use kvm_ioctls::{Kvm, VcpuExit, VcpuFd, VmFd};

pub use cella_libs::sidecar::{IrqChipState, VcpuState, SAVED_MSRS, SAVED_MSR_COUNT};

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
    // Read all other state first, and read the MSRs last. The MSRs
    // contain MSR_IA32_TSC. main.rs reads the kvmclock immediately after
    // this function. The TSC and the kvmclock must be read close together
    // in time, because the thaw writes them close together in time.
    //
    // An earlier version read the MSRs first. The code then read the TSC
    // approximately 8 ioctls before the clock, and one of those ioctls
    // copies 4 KB of xsave data.
    //
    // This sequence is correct in principle, but do not record it as a
    // fix. The clocksource watchdog in the guest still marks the TSC
    // unstable after a thaw. Measurements of the skew give 2, 4, 5, 13,
    // 19, 23, and 25 ms, before and after this change. The values vary
    // more than the change does, so the change shows no measured effect.
    // The permitted skew is less than 1 ms. The cause of the remaining
    // skew is not known. This is an open item.
    let regs = vcpu.get_regs()?;
    let sregs = vcpu.get_sregs()?;
    let fpu = vcpu.get_fpu()?;
    let mp_state = vcpu.get_mp_state()?;
    let lapic = vcpu.get_lapic()?;
    let events = vcpu.get_vcpu_events()?;
    let xsave_region = vcpu.get_xsave()?.region;
    let xcrs = vcpu.get_xcrs()?;

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
    // KVM_GET_MSRS gives the number of entries that it read. It stops at
    // the first entry that it cannot read, and it does not change the
    // entries after it. A short read therefore gives an incomplete set of
    // registers. Report this condition as an error.
    let read = vcpu.get_msrs(&mut msrs_req)?;
    if read != SAVED_MSR_COUNT {
        return Err(Error::MsrCountMismatch);
    }

    let mut msrs = [(0u32, 0u64); SAVED_MSR_COUNT];
    for (slot, e) in msrs.iter_mut().zip(msrs_req.as_slice()) {
        *slot = (e.index, e.data);
    }

    Ok(VcpuState {
        regs,
        sregs,
        fpu,
        mp_state,
        lapic,
        events,
        msrs,
        xsave_region,
        xcrs,
    })
}

/// Restore all state that `save` read. The sequence is important. Write
/// sregs and regs before the MSRs. Write the MSRs last, because the other
/// writes can change the TSC. In the MSR batch, the sequence is the
/// sequence of SAVED_MSRS. MSR_IA32_TSC is the first entry. Therefore the
/// code writes the TSC-deadline value at the end, when the TSC is
/// correct.
///
/// This direct MSR write of the TSC is correct only because there is one
/// vCPU. KVM has TSC synchronization heuristics that keep the counters of
/// two or more vCPUs aligned. These heuristics do not apply here.
pub fn restore(vcpu: &VcpuFd, state: &VcpuState) -> Result<(), Error> {
    vcpu.set_sregs(&state.sregs)?;
    vcpu.set_regs(&state.regs)?;
    vcpu.set_fpu(&state.fpu)?;
    vcpu.set_mp_state(state.mp_state)?;
    vcpu.set_lapic(&state.lapic)?;
    vcpu.set_vcpu_events(&state.events)?;
    // Write XCR0 before the xsave buffer. KVM uses the current XCR0 of
    // the guest to select which components of the buffer to load. If XCR0
    // is not correct, KVM does not load the AVX state.
    vcpu.set_xcrs(&state.xcrs)?;
    // kvm_xsave contains a flexible array member for the components that
    // KVM can add, for example AMX. Therefore the type is not Copy and
    // the code cannot keep it by value. The code keeps the fixed region
    // of 4096 bytes. The xstate of this guest is 0x2e7 and uses 2440
    // bytes, thus the region is sufficient.
    // SAFETY: a kvm_xsave that contains only zeros is valid. The code
    // then writes the region. For a guest that has no added components,
    // the region is the only part that KVM reads.
    let mut xs: kvm_xsave = unsafe { std::mem::zeroed() };
    xs.region = state.xsave_region;
    vcpu.set_xsave(&xs)?;

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
    // Use the same check as `save`. If KVM writes only some of the
    // entries, the guest continues with some registers from the sidecar
    // and some registers from a new vCPU.
    let written = vcpu.set_msrs(&msrs_req)?;
    if written != entries.len() {
        return Err(Error::MsrCountMismatch);
    }
    Ok(())
}

// The nested virtualization state. A guest can run KVM itself, and the
// state of its inner VM lives partly in the host kernel (the VMCS or
// VMCB cache), not in guest RAM. A freeze that drops this state makes
// the next VM entry of the thawed guest fail, and the guest triple
// faults. kvm-ioctls 0.19 has no wrapper for these two ioctls, thus
// the raw numbers stand here: _IOWR/_IOW(KVMIO=0xAE, 0xbe/0xbf,
// struct kvm_nested_state) with the 128-byte fixed header.
const KVM_GET_NESTED_STATE: libc::c_ulong = 0xc080_aebe;
const KVM_SET_NESTED_STATE: libc::c_ulong = 0x4080_aebf;

/// Read the nested state as raw bytes: the fixed header plus the
/// variable data region. An empty vector means the host offers no
/// nested state (the capability is absent), and the thaw skips the
/// restore.
pub fn save_nested(vcpu: &VcpuFd) -> Result<Vec<u8>, Error> {
    use std::os::unix::io::AsRawFd;
    let hdr = std::mem::size_of::<kvm_bindings::kvm_nested_state>();
    let mut buf = vec![0u8; hdr];
    loop {
        let cap = buf.len() as u32;
        buf[4..8].copy_from_slice(&cap.to_le_bytes());
        // SAFETY: buf holds at least the fixed header, and its size
        // field tells the kernel the capacity of the buffer.
        let r = unsafe { libc::ioctl(vcpu.as_raw_fd(), KVM_GET_NESTED_STATE, buf.as_mut_ptr()) };
        if r == 0 {
            let used = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
            buf.truncate(used.clamp(hdr, buf.len()));
            return Ok(buf);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            // The state does not fit: the kernel wrote the required
            // size into the size field. Grow and retry.
            Some(libc::E2BIG) => {
                let need = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
                buf = vec![0u8; need.max(hdr)];
            }
            // The host has no nested-state capability.
            Some(libc::ENOTTY) | Some(libc::EINVAL) => return Ok(Vec::new()),
            _ => return Err(Error::Kvm(kvm_ioctls::Error::last())),
        }
    }
}

/// The MSRs of nested virtualization, per vendor. On AMD,
/// MSR_VM_HSAVE_PA holds the host-save area that VMRUN uses: a thaw
/// that zeroes it makes the inner KVM's next VMRUN triple fault the
/// guest. On Intel, IA32_FEATURE_CONTROL carries the locked VMX
/// enable bit that the guest kernel sets at boot. Each MSR exists on
/// one vendor alone, thus none can join SAVED_MSRS (a strict list
/// that every host must read in full). `save_nested_msrs` probes
/// each one and keeps the readable set.
const NESTED_MSRS: &[u32] = &[
    0x0000_003a, // MSR_IA32_FEATURE_CONTROL (Intel)
    0xc001_0114, // MSR_VM_CR (AMD)
    0xc001_0117, // MSR_VM_HSAVE_PA (AMD)
];

/// Read the nested-virtualization MSRs that this host offers. An MSR
/// the host cannot read is left out, not an error.
pub fn save_nested_msrs(vcpu: &VcpuFd) -> Vec<(u32, u64)> {
    let mut out = Vec::new();
    for &index in NESTED_MSRS {
        let mut req = Msrs::from_entries(&[kvm_msr_entry {
            index,
            ..Default::default()
        }])
        .expect("one entry is well-formed");
        if let Ok(1) = vcpu.get_msrs(&mut req) {
            out.push((index, req.as_slice()[0].data));
        }
    }
    out
}

/// Write the nested-virtualization MSRs back, before the first
/// KVM_RUN.
pub fn restore_nested_msrs(vcpu: &VcpuFd, msrs: &[(u32, u64)]) -> Result<(), Error> {
    if msrs.is_empty() {
        return Ok(());
    }
    let entries: Vec<kvm_msr_entry> = msrs
        .iter()
        .map(|&(index, data)| kvm_msr_entry {
            index,
            data,
            ..Default::default()
        })
        .collect();
    let req = Msrs::from_entries(&entries).map_err(|_| Error::MsrCountMismatch)?;
    if vcpu.set_msrs(&req)? != entries.len() {
        return Err(Error::MsrCountMismatch);
    }
    Ok(())
}

/// Write the nested state back, before the first KVM_RUN. The bytes
/// are exactly what `save_nested` read.
pub fn restore_nested(vcpu: &VcpuFd, data: &[u8]) -> Result<(), Error> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: data begins with the fixed header that save_nested wrote,
    // and its size field covers the whole slice.
    let r = unsafe { libc::ioctl(vcpu.as_raw_fd(), KVM_SET_NESTED_STATE, data.as_ptr()) };
    if r == 0 {
        Ok(())
    } else {
        Err(Error::Kvm(kvm_ioctls::Error::last()))
    }
}

// The KVM_IRQCHIP_* chip ids. The code gives them names here, so that
// the three calls below are easy to read.
const CHIP_PIC_MASTER: u32 = 0;
const CHIP_PIC_SLAVE: u32 = 1;
const CHIP_IOAPIC: u32 = 2;

fn get_chip(vm: &VmFd, chip_id: u32) -> Result<kvm_irqchip, Error> {
    let mut chip = kvm_irqchip {
        chip_id,
        ..Default::default()
    };
    vm.get_irqchip(&mut chip)?;
    Ok(chip)
}

pub fn save_irqchip(vm: &VmFd) -> Result<IrqChipState, Error> {
    Ok(IrqChipState {
        pic_master: get_chip(vm, CHIP_PIC_MASTER)?,
        pic_slave: get_chip(vm, CHIP_PIC_SLAVE)?,
        ioapic: get_chip(vm, CHIP_IOAPIC)?,
        pit: vm.get_pit2()?,
    })
}

pub fn restore_irqchip(vm: &VmFd, state: &IrqChipState) -> Result<(), Error> {
    vm.set_irqchip(&state.pic_master)?;
    vm.set_irqchip(&state.pic_slave)?;
    vm.set_irqchip(&state.ioapic)?;
    vm.set_pit2(&state.pit)?;
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
        VcpuExit::Shutdown => RunResult::Shutdown,
        VcpuExit::FailEntry(reason, cpu) => {
            eprintln!("cella: KVM entry failed: reason {reason:#x} on cpu {cpu}");
            RunResult::Shutdown
        }
        VcpuExit::Intr | VcpuExit::IrqWindowOpen => RunResult::Continue,
        _ => RunResult::Continue,
    }
}
