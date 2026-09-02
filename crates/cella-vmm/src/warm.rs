//! Deep stage-2 warming at thaw.
//!
//! KVM_PRE_FAULT_MEMORY fills the stage-2 tables of the host that runs
//! this process. Under a nested stack, each layer below builds its own
//! combined mapping, and it builds that mapping on the first access of
//! the guest. No ioctl at any layer reaches those mappings. A real
//! guest access does: the architecture forces every layer to resolve
//! the translation. probe-inception measured the cost of the lazy
//! path: ~4 ms for each nesting level, and it compounds with depth.
//!
//! This module therefore runs a throwaway stub on the fresh vCPU,
//! before the restore of the vCPU state and of the clock. The stub
//! reads one byte from each page of guest RAM and writes the same
//! byte back, thus the read path and the write path both warm. The
//! cost lands in host time, outside the clock window of the guest.
//!
//! The stub must not touch the frozen RAM image, thus its code, its
//! page tables, and its stack live in a scratch memslot at
//! SCRATCH_GPA. The scratch slot stays in place after the warming:
//! the deletion of a memslot makes KVM zap all roots, and that would
//! discard the warmed mappings. The guest never addresses the scratch
//! range (pci=off, and the RAM of the guest ends far below it).
//!
//! The stub exits with OUT to EXIT_PORT. HLT would not return to this
//! process: with the in-kernel irqchip an idle vCPU blocks inside
//! KVM_RUN.

use kvm_bindings::{kvm_regs, kvm_segment, kvm_sregs, kvm_userspace_memory_region};
use kvm_ioctls::{VcpuExit, VcpuFd, VmFd};

/// The guest-physical base of the scratch memslot. 3 GiB: above any
/// test guest's RAM, below nothing the guest addresses.
const SCRATCH_GPA: u64 = 0xc000_0000;
const SCRATCH_SLOT: u32 = 31;
/// Four pages: PML4, PDPT, stub code, stub stack.
const SCRATCH_SIZE: u64 = 0x4000;
const EXIT_PORT: u16 = 0xf4;

/// The stub. Entry state: RAX = 0, RCX = the RAM size in bytes.
///   loop: mov bl, [rax]
///         mov [rax], bl
///         add rax, 4096
///         cmp rax, rcx
///         jb loop
///         out EXIT_PORT, al
///         hlt
const STUB: &[u8] = &[
    0x8a, 0x18, // mov bl, [rax]
    0x88, 0x18, // mov [rax], bl
    0x48, 0x05, 0x00, 0x10, 0x00, 0x00, // add rax, 0x1000
    0x48, 0x39, 0xc8, // cmp rax, rcx
    0x72, 0xf1, // jb loop
    0xe6, 0xf4, // out 0xf4, al
    0xf4, // hlt
];

fn seg(selector: u16, type_: u8) -> kvm_segment {
    kvm_segment {
        base: 0,
        limit: 0xffff_ffff,
        selector,
        type_,
        present: 1,
        dpl: 0,
        db: 0,
        s: 1,
        l: 1,
        g: 1,
        avl: 0,
        unusable: 0,
        padding: 0,
    }
}

/// Run the warming stub. `mem_size` is the size of guest RAM. Returns
/// false when the warming could not run; the thaw then continues with
/// the lazy path, which is slower for the guest but correct.
pub fn warm_stage2(vm: &VmFd, vcpu: &mut VcpuFd, mem_size: u64) -> bool {
    if mem_size > SCRATCH_GPA {
        eprintln!("cella: warm: guest RAM reaches the scratch range, warming skipped");
        return false;
    }

    // Host pages for the scratch slot. Kept for the process lifetime:
    // see the module comment on memslot deletion.
    // SAFETY: an anonymous private mapping of a fixed small size.
    let host = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            SCRATCH_SIZE as usize,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if host == libc::MAP_FAILED {
        eprintln!("cella: warm: scratch mmap failed, warming skipped");
        return false;
    }

    // Identity page tables in the scratch pages: PML4 at +0, PDPT at
    // +0x1000, and four 1 GiB entries covering 0..4 GiB. The stub and
    // the RAM of the guest both lie inside that range.
    // SAFETY: host points at SCRATCH_SIZE writable bytes.
    unsafe {
        let pml4 = host as *mut u64;
        *pml4 = (SCRATCH_GPA + 0x1000) | 0x3; // present | write
        let pdpt = host.add(0x1000) as *mut u64;
        for i in 0..4u64 {
            *pdpt.add(i as usize) = (i << 30) | 0x83; // present | write | 1 GiB
        }
        std::ptr::copy_nonoverlapping(STUB.as_ptr(), host.add(0x2000) as *mut u8, STUB.len());
    }

    // SAFETY: the mapping outlives the process's use of the slot (it is
    // never unmapped), and the region does not overlap guest RAM.
    let set = unsafe {
        vm.set_user_memory_region(kvm_userspace_memory_region {
            slot: SCRATCH_SLOT,
            guest_phys_addr: SCRATCH_GPA,
            memory_size: SCRATCH_SIZE,
            userspace_addr: host as u64,
            flags: 0,
        })
    };
    if set.is_err() {
        eprintln!("cella: warm: scratch memslot rejected, warming skipped");
        return false;
    }

    // Long mode with cached descriptors: KVM takes the hidden segment
    // state directly, thus no GDT lives in memory (probe-sregs covers
    // the ordering constraints of this ioctl).
    let mut sregs: kvm_sregs = match vcpu.get_sregs() {
        Ok(s) => s,
        Err(_) => return false,
    };
    sregs.cr0 = 0x8005_0033; // PE | MP | ET | NE | WP | AM | PG
    sregs.cr3 = SCRATCH_GPA;
    sregs.cr4 = 0x20; // PAE
    sregs.efer = 0x500; // LME | LMA
    sregs.cs = seg(0x8, 0xb);
    let data = seg(0x10, 0x3);
    sregs.ds = data;
    sregs.es = data;
    sregs.ss = data;
    if vcpu.set_sregs(&sregs).is_err() {
        eprintln!("cella: warm: set_sregs failed, warming skipped");
        return false;
    }
    let regs = kvm_regs {
        rip: SCRATCH_GPA + 0x2000,
        rsp: SCRATCH_GPA + 0x3ff8,
        rax: 0,
        rcx: mem_size,
        rflags: 0x2,
        ..Default::default()
    };
    if vcpu.set_regs(&regs).is_err() {
        eprintln!("cella: warm: set_regs failed, warming skipped");
        return false;
    }

    let t = std::time::Instant::now();
    loop {
        match vcpu.run() {
            Ok(VcpuExit::IoOut(port, _)) if port == EXIT_PORT => break,
            Ok(other) => {
                eprintln!("cella: warm: unexpected exit {other:?}, warming stopped");
                return false;
            }
            Err(e) if e.errno() == libc::EINTR => continue,
            Err(e) => {
                eprintln!("cella: warm: KVM_RUN failed: {e}, warming stopped");
                return false;
            }
        }
    }
    eprintln!(
        "cella: thaw timing: warm(stage-2, all layers) {} pages in {} ns ({}.{:09} s)",
        mem_size / 4096,
        t.elapsed().as_nanos(),
        t.elapsed().as_secs(),
        t.elapsed().subsec_nanos()
    );
    true
}
