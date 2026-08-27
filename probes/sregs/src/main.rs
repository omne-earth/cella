//! Standalone probe for the setup_gdt/setup_long_mode KVM_SET_SREGS
//! ordering bug (see src/boot/x86_64.rs). Not part of the cella crate --
//! deliberately independent so different orderings can be tried against
//! real KVM without touching the real boot path until one is known to
//! work.
//!
//! `make smoke-boot` failed with `gdt: Kvm(Error(22))` (EINVAL). The
//! hypothesis: setup_gdt's KVM_SET_SREGS call sets CS.L=1 (a 64-bit code
//! segment) while EFER.LMA/CR0.PG are still off (they're only enabled
//! later, in setup_long_mode's *separate* KVM_SET_SREGS call) -- an
//! inconsistent state KVM's validation rejects. Three attempts below
//! check that hypothesis and two candidate fixes.
//!
//! Run: cargo run --manifest-path probes/sregs/Cargo.toml

use kvm_bindings::{kvm_pit_config, kvm_segment, kvm_sregs, CpuId, KVM_MAX_CPUID_ENTRIES, KVM_MP_STATE_RUNNABLE};
use kvm_ioctls::{Kvm, VcpuFd, VmFd};

const X86_CR0_PE: u64 = 1 << 0;
const X86_CR0_ET: u64 = 1 << 4;
const X86_CR0_WP: u64 = 1 << 16;
const X86_CR0_PG: u64 = 1 << 31;
const X86_CR4_PAE: u64 = 1 << 5;
const EFER_LME: u64 = 1 << 8;
const EFER_LMA: u64 = 1 << 10;

const BOOT_GDT_OFFSET: u64 = 0x500;
const PML4_START: u64 = 0x9000; // arbitrary, unused by SET_SREGS validation itself

/// Mirrors boot/x86_64.rs's gdt_entry/kvm_segment_from_gdt exactly
/// (duplicated here on purpose -- this probe stays independent of the
/// real boot path until a fix is confirmed).
fn gdt_entry(flags: u16, base: u32, limit: u32) -> u64 {
    ((u64::from(base) & 0xff00_0000) << 32)
        | (u64::from(flags) << 40)
        | ((u64::from(limit) & 0x000f_0000) << 32)
        | ((u64::from(base) & 0x00ff_ffff) << 16)
        | (u64::from(limit) & 0x0000_ffff)
}

fn kvm_segment_from_gdt(entry: u64, selector: u16) -> kvm_segment {
    let base = (((entry) & 0xff00_0000_0000_0000) >> 32)
        | ((entry & 0x0000_00ff_0000_0000) >> 16)
        | ((entry & 0x0000_0000_ffff_0000) >> 16);
    let limit = ((entry & 0x000f_0000_0000_0000) >> 32) | (entry & 0x0000_0000_0000_ffff);
    let g = (entry & (1 << 55)) != 0;
    let db = (entry & (1 << 54)) != 0;
    let l = (entry & (1 << 53)) != 0;
    let avl = (entry & (1 << 52)) != 0;
    let present = (entry & (1 << 47)) != 0;
    let dpl = ((entry >> 45) & 0x3) as u8;
    let seg_type = ((entry >> 40) & 0xf) as u8;
    let s = (entry & (1 << 44)) != 0;

    kvm_segment {
        base,
        limit: limit as u32,
        selector: selector << 3,
        type_: seg_type,
        present: present as u8,
        dpl,
        db: db as u8,
        s: s as u8,
        l: l as u8,
        g: g as u8,
        avl: avl as u8,
        unusable: 0,
        padding: 0,
    }
}

fn fresh_vcpu(kvm: &Kvm) -> (VmFd, VcpuFd) {
    let vm = kvm.create_vm().expect("KVM_CREATE_VM");
    vm.set_tss_address(0xffff_d000).unwrap();
    vm.set_identity_map_address(0xffff_c000).unwrap();
    vm.create_irq_chip().expect("KVM_CREATE_IRQCHIP");
    vm.create_pit2(kvm_pit_config::default())
        .expect("KVM_CREATE_PIT2");

    let cpuid: CpuId = kvm
        .get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)
        .expect("KVM_GET_SUPPORTED_CPUID");
    let vcpu = vm.create_vcpu(0).expect("KVM_CREATE_VCPU");
    vcpu.set_cpuid2(&cpuid).expect("KVM_SET_CPUID2");
    let mut mp = vcpu.get_mp_state().expect("KVM_GET_MP_STATE");
    mp.mp_state = KVM_MP_STATE_RUNNABLE;
    vcpu.set_mp_state(mp).expect("KVM_SET_MP_STATE");

    (vm, vcpu)
}

fn base_gdt_segments() -> (kvm_segment, kvm_segment) {
    let gdt_table: [u64; 4] = [
        gdt_entry(0, 0, 0),
        gdt_entry(0xa09b, 0, 0xfffff), // 64-bit code: present, ring0, exec/read, L=1
        gdt_entry(0xc093, 0, 0xfffff), // data: present, ring0, read/write
        gdt_entry(0, 0, 0),
    ];
    (
        kvm_segment_from_gdt(gdt_table[1], 1),
        kvm_segment_from_gdt(gdt_table[2], 2),
    )
}

fn report(name: &str, result: Result<(), kvm_ioctls::Error>) {
    match result {
        Ok(()) => println!("{name}: OK (KVM_SET_SREGS accepted)"),
        Err(e) => println!("{name}: REJECTED -- {e}"),
    }
}

/// Attempt A: reproduce the bug as-is. CS.L=1 set with paging/EFER still
/// off, in a single KVM_SET_SREGS call -- the current setup_gdt behavior.
fn attempt_a_reproduce_bug(kvm: &Kvm) {
    let (_vm, vcpu) = fresh_vcpu(kvm);
    let (code_seg, data_seg) = base_gdt_segments();

    let mut sregs: kvm_sregs = vcpu.get_sregs().expect("KVM_GET_SREGS");
    sregs.gdt.base = BOOT_GDT_OFFSET;
    sregs.gdt.limit = 31;
    sregs.cs = code_seg;
    sregs.ds = data_seg;
    sregs.es = data_seg;
    sregs.fs = data_seg;
    sregs.gs = data_seg;
    sregs.ss = data_seg;
    sregs.cr0 |= X86_CR0_PE | X86_CR0_ET | X86_CR0_WP;
    // Note: no CR4.PAE, no CR0.PG, no EFER.LME/LMA yet.

    report("A (reproduce bug: CS.L=1 before paging/EFER)", vcpu.set_sregs(&sregs).map(|_| ()));
}

/// Attempt B: the merged fix. Everything -- GDT/segments (CS.L=1) *and*
/// CR3/CR4.PAE/CR0.PG/EFER.LME|LMA -- lands in one KVM_SET_SREGS call.
fn attempt_b_merged(kvm: &Kvm) {
    let (_vm, vcpu) = fresh_vcpu(kvm);
    let (code_seg, data_seg) = base_gdt_segments();

    let mut sregs: kvm_sregs = vcpu.get_sregs().expect("KVM_GET_SREGS");
    sregs.gdt.base = BOOT_GDT_OFFSET;
    sregs.gdt.limit = 31;
    sregs.cs = code_seg;
    sregs.ds = data_seg;
    sregs.es = data_seg;
    sregs.fs = data_seg;
    sregs.gs = data_seg;
    sregs.ss = data_seg;
    sregs.cr0 |= X86_CR0_PE | X86_CR0_ET | X86_CR0_WP | X86_CR0_PG;
    sregs.cr3 = PML4_START;
    sregs.cr4 |= X86_CR4_PAE;
    sregs.efer |= EFER_LME | EFER_LMA;

    report("B (merged: segments + paging/EFER in one call)", vcpu.set_sregs(&sregs).map(|_| ()));
}

/// Attempt C: staged but reordered. First call enables paging/EFER only
/// (CS left as KVM's power-on default, L=0). Second call then sets
/// CS.L=1 now that EFER.LMA is already 1. Checks whether a smaller diff
/// (reorder setup_long_mode's paging step before setup_gdt's segment
/// step, keep two calls) is viable instead of a full merge.
fn attempt_c_staged_reordered(kvm: &Kvm) {
    let (_vm, vcpu) = fresh_vcpu(kvm);
    let (code_seg, data_seg) = base_gdt_segments();

    let mut sregs: kvm_sregs = vcpu.get_sregs().expect("KVM_GET_SREGS");
    sregs.cr0 |= X86_CR0_PE | X86_CR0_ET | X86_CR0_WP | X86_CR0_PG;
    sregs.cr3 = PML4_START;
    sregs.cr4 |= X86_CR4_PAE;
    sregs.efer |= EFER_LME | EFER_LMA;
    let step1 = vcpu.set_sregs(&sregs).map(|_| ());
    report("C step 1 (paging/EFER only, CS untouched)", step1);
    if step1.is_err() {
        return;
    }

    let mut sregs: kvm_sregs = vcpu.get_sregs().expect("KVM_GET_SREGS");
    sregs.gdt.base = BOOT_GDT_OFFSET;
    sregs.gdt.limit = 31;
    sregs.cs = code_seg;
    sregs.ds = data_seg;
    sregs.es = data_seg;
    sregs.fs = data_seg;
    sregs.gs = data_seg;
    sregs.ss = data_seg;
    report(
        "C step 2 (segments/CS.L=1, paging/EFER already on)",
        vcpu.set_sregs(&sregs).map(|_| ()),
    );
}

/// Attempt D: the actual minimal-diff candidate -- just swap the call
/// order in main.rs (setup_long_mode's sregs write before setup_gdt's),
/// zero other changes. Unlike attempt C, step 1 here sets *only*
/// CR0.PG|CR3|CR4.PAE|EFER (setup_long_mode's exact current code), not
/// PE/ET/WP too -- so if CR0.PE=0 turns out to itself be inconsistent
/// with EFER.LMA=1, this (and not C) is what would catch it.
fn attempt_d_exact_reorder(kvm: &Kvm) {
    let (_vm, vcpu) = fresh_vcpu(kvm);
    let (code_seg, data_seg) = base_gdt_segments();

    // setup_long_mode's current code, verbatim, run first.
    let mut sregs: kvm_sregs = vcpu.get_sregs().expect("KVM_GET_SREGS");
    sregs.cr3 = PML4_START;
    sregs.cr4 |= X86_CR4_PAE;
    sregs.cr0 |= X86_CR0_PG;
    sregs.efer |= EFER_LME | EFER_LMA;
    let step1 = vcpu.set_sregs(&sregs).map(|_| ());
    report("D step 1 (setup_long_mode's sregs bits, verbatim, run first)", step1);
    if step1.is_err() {
        return;
    }

    // setup_gdt's current code, verbatim, run second.
    let mut sregs: kvm_sregs = vcpu.get_sregs().expect("KVM_GET_SREGS");
    sregs.gdt.base = BOOT_GDT_OFFSET;
    sregs.gdt.limit = 31;
    sregs.cs = code_seg;
    sregs.ds = data_seg;
    sregs.es = data_seg;
    sregs.fs = data_seg;
    sregs.gs = data_seg;
    sregs.ss = data_seg;
    sregs.cr0 |= X86_CR0_PE | X86_CR0_ET | X86_CR0_WP;
    report(
        "D step 2 (setup_gdt's sregs bits, verbatim, run second)",
        vcpu.set_sregs(&sregs).map(|_| ()),
    );
}

fn main() {
    let kvm = Kvm::new().expect("open /dev/kvm");

    attempt_a_reproduce_bug(&kvm);
    attempt_b_merged(&kvm);
    attempt_c_staged_reordered(&kvm);
    attempt_d_exact_reorder(&kvm);
}
