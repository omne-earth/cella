//! x86_64 direct kernel boot.
//!
//! We boot a bzImage straight into 64-bit mode, the same way Firecracker,
//! Cloud Hypervisor, and rust-vmm's own vmm-reference do it: build a GDT
//! and identity-mapped page tables ourselves, hand-craft the CPU state for
//! long mode, and jump to the kernel's 64-bit entry point
//! (`load_addr + 0x200`), which per the Linux boot protocol assumes long
//! mode, paging, and a flat GDT are already set up by the loader.
//!
//! No firmware, no BIOS, no real-mode trampoline: this is the entire boot
//! path.

use std::fs::File;
use std::io;
use std::path::Path;

use kvm_bindings::{kvm_regs, kvm_segment, kvm_sregs};
use kvm_ioctls::VcpuFd;
use linux_loader::bootparam::{boot_e820_entry, boot_params};
use linux_loader::configurator::linux::LinuxBootConfigurator;
use linux_loader::configurator::{BootConfigurator, BootParams};
use linux_loader::loader::{BzImage, Cmdline, KernelLoader, KernelLoaderResult};
use vm_memory::{Address, Bytes, GuestAddress, GuestMemoryMmap};

const BOOT_GDT_OFFSET: u64 = 0x500;
const BOOT_STACK_POINTER: u64 = 0x8ff0;
const ZERO_PAGE_START: u64 = 0x7000;
const CMDLINE_START: u64 = 0x2_0000;
const CMDLINE_MAX_SIZE: usize = 0x1_0000;
const PML4_START: u64 = 0x9000;
const PDPTE_START: u64 = 0xa000;
const PDE_START: u64 = 0xb000; // one 4KiB PDE table per GiB of identity-mapped RAM
const HIMEM_START: u64 = 0x10_0000; // 1 MiB: bzImage protected-mode code load address

const KERNEL_BOOT_FLAG_MAGIC: u16 = 0xaa55;
const KERNEL_HDR_MAGIC: u32 = 0x5372_6448; // "HdrS"
const KERNEL_LOADER_OTHER: u8 = 0xff;
const KERNEL_MIN_ALIGNMENT_BYTES: u32 = 0x0100_0000;
const E820_RAM: u32 = 1;

const X86_CR0_PE: u64 = 1 << 0;
const X86_CR0_PG: u64 = 1 << 31;
const X86_CR0_ET: u64 = 1 << 4; // required on some KVM versions for KVM_SET_SREGS to accept CR0
const X86_CR0_WP: u64 = 1 << 16;
const X86_CR4_PAE: u64 = 1 << 5;
const EFER_LME: u64 = 1 << 8; // long mode enable
const EFER_LMA: u64 = 1 << 10; // long mode active

const MAX_ADDRESSABLE_1G_PAGES: u64 = 4; // identity map up to 4 GiB of guest RAM

#[allow(dead_code)] // fields read via {:?} in error messages, not field access
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Loader(linux_loader::loader::Error),
    Configurator(linux_loader::configurator::Error),
    Cmdline(linux_loader::cmdline::Error),
    Kvm(kvm_ioctls::Error),
    MemoryTooLarge,
    GuestMemory,
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}
impl From<kvm_ioctls::Error> for Error {
    fn from(e: kvm_ioctls::Error) -> Self {
        Error::Kvm(e)
    }
}

/// Result of loading the kernel: where it ended up, and what the vCPU's
/// initial RIP/RSI should be.
pub struct BootInfo {
    pub entry_point: GuestAddress,
    pub zero_page: GuestAddress,
}

/// Load a bzImage kernel and command line into guest memory, and write the
/// zero page (boot_params) the 64-bit Linux boot protocol expects.
pub fn load_kernel(
    mem: &GuestMemoryMmap,
    kernel_path: &Path,
    cmdline_str: &str,
    guest_mem_size: u64,
) -> Result<BootInfo, Error> {
    let mut kernel_file = File::open(kernel_path)?;

    let loader_result: KernelLoaderResult =
        BzImage::load(mem, None, &mut kernel_file, Some(GuestAddress(HIMEM_START)))
            .map_err(Error::Loader)?;

    let mut cmdline = Cmdline::new(CMDLINE_MAX_SIZE).map_err(Error::Cmdline)?;
    cmdline.insert_str(cmdline_str).map_err(Error::Cmdline)?;
    linux_loader::loader::load_cmdline(mem, GuestAddress(CMDLINE_START), &cmdline)
        .map_err(Error::Loader)?;

    let mut params = boot_params::default();
    if let Some(hdr) = loader_result.setup_header {
        params.hdr = hdr;
    }
    params.hdr.type_of_loader = KERNEL_LOADER_OTHER;
    params.hdr.boot_flag = KERNEL_BOOT_FLAG_MAGIC;
    params.hdr.header = KERNEL_HDR_MAGIC;
    params.hdr.cmd_line_ptr = CMDLINE_START as u32;
    params.hdr.cmdline_size = cmdline_str.len() as u32 + 1;
    params.hdr.kernel_alignment = KERNEL_MIN_ALIGNMENT_BYTES;
    // No initrd: this design boots a self-contained kernel with the root
    // filesystem attached over virtio-blk instead.
    params.hdr.ramdisk_image = 0;
    params.hdr.ramdisk_size = 0;

    add_e820_entry(&mut params, 0, HIMEM_START, E820_RAM);
    add_e820_entry(
        &mut params,
        HIMEM_START,
        guest_mem_size - HIMEM_START,
        E820_RAM,
    );

    let boot_params = BootParams::new(&params, GuestAddress(ZERO_PAGE_START));
    LinuxBootConfigurator::write_bootparams(&boot_params, mem).map_err(Error::Configurator)?;

    // bzImage 64-bit entry point per Documentation/x86/boot.rst: the start
    // of the protected-mode kernel image, plus 0x200. `kernel_load` is
    // where BzImage::load() placed that image.
    let entry_point = GuestAddress(loader_result.kernel_load.raw_value() + 0x200);

    Ok(BootInfo {
        entry_point,
        zero_page: GuestAddress(ZERO_PAGE_START),
    })
}

fn add_e820_entry(params: &mut boot_params, addr: u64, size: u64, typ: u32) {
    let i = params.e820_entries as usize;
    if i >= params.e820_table.len() {
        return;
    }
    params.e820_table[i] = boot_e820_entry {
        addr,
        size,
        type_: typ,
    };
    params.e820_entries += 1;
}

/// Build a minimal flat GDT (null, code64, data64) at BOOT_GDT_OFFSET and
/// point the vCPU's segment registers at it.
pub fn setup_gdt(mem: &GuestMemoryMmap, vcpu: &VcpuFd) -> Result<(), Error> {
    // Raw 8-byte GDT descriptors. Index 1 = code64, index 2 = data.
    let gdt_table: [u64; 4] = [
        gdt_entry(0, 0, 0),            // null
        gdt_entry(0xa09b, 0, 0xfffff), // 64-bit code: present, ring0, exec/read, L=1
        gdt_entry(0xc093, 0, 0xfffff), // data: present, ring0, read/write
        gdt_entry(0, 0, 0),            // unused
    ];
    for (i, entry) in gdt_table.iter().enumerate() {
        mem.write_obj(*entry, GuestAddress(BOOT_GDT_OFFSET + (i as u64) * 8))
            .map_err(|_| Error::GuestMemory)?;
    }

    let mut sregs: kvm_sregs = vcpu.get_sregs()?;

    sregs.gdt.base = BOOT_GDT_OFFSET;
    sregs.gdt.limit = (std::mem::size_of_val(&gdt_table) - 1) as u16;

    let code_seg = kvm_segment_from_gdt(gdt_table[1], 1);
    let data_seg = kvm_segment_from_gdt(gdt_table[2], 2);

    sregs.cs = code_seg;
    sregs.ds = data_seg;
    sregs.es = data_seg;
    sregs.fs = data_seg;
    sregs.gs = data_seg;
    sregs.ss = data_seg;

    sregs.cr0 |= X86_CR0_PE | X86_CR0_ET;
    sregs.cr0 |= X86_CR0_WP;

    vcpu.set_sregs(&sregs)?;
    Ok(())
}

fn gdt_entry(flags: u16, base: u32, limit: u32) -> u64 {
    // Standard x86 segment descriptor packing. `flags` carries the access
    // byte in bits [8:15] plus the granularity/L/D bits we care about in
    // bits above that, pre-baked per call site above rather than
    // decomposed further -- there are only two entries, so a full
    // bitfield builder would cost more lines than it saves.
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

/// Build identity-mapped page tables (2 MiB pages) covering `mem_size`
/// bytes. Pure function of guest memory -- no vCPU involved -- so it's
/// unit-testable without KVM; see the `tests` module below.
pub fn build_page_tables(mem: &GuestMemoryMmap, mem_size: u64) -> Result<(), Error> {
    let gib = mem_size.div_ceil(1 << 30).max(1);
    if gib > MAX_ADDRESSABLE_1G_PAGES {
        return Err(Error::MemoryTooLarge);
    }

    // PML4[0] -> PDPTE table
    write_pte(mem, PML4_START, 0, PDPTE_START | 0x03)?;

    for g in 0..gib {
        let pde_table = PDE_START + g * 0x1000;
        // PDPTE[g] -> this GiB's PDE table
        write_pte(mem, PDPTE_START, g, pde_table | 0x03)?;
        for p in 0..512u64 {
            let addr = g * (1 << 30) + p * (1 << 21);
            // Present, writable, huge page (2 MiB).
            write_pte(mem, pde_table, p, addr | 0x83)?;
        }
    }
    Ok(())
}

/// Load CR3/CR4/EFER for long mode and set RIP/RSP/RSI/RFLAGS for the
/// 64-bit Linux boot entry. Calls `build_page_tables` first.
pub fn setup_long_mode(
    mem: &GuestMemoryMmap,
    vcpu: &VcpuFd,
    boot: &BootInfo,
    mem_size: u64,
) -> Result<(), Error> {
    build_page_tables(mem, mem_size)?;

    let mut sregs: kvm_sregs = vcpu.get_sregs()?;
    sregs.cr3 = PML4_START;
    sregs.cr4 |= X86_CR4_PAE;
    sregs.cr0 |= X86_CR0_PG;
    sregs.efer |= EFER_LME | EFER_LMA;
    vcpu.set_sregs(&sregs)?;

    let regs = kvm_regs {
        rflags: 0x2, // bit 1 is reserved-as-1
        rip: boot.entry_point.raw_value(),
        rsp: BOOT_STACK_POINTER,
        rbp: BOOT_STACK_POINTER,
        rsi: boot.zero_page.raw_value(), // Linux boot protocol: RSI = boot_params ptr
        ..Default::default()
    };
    vcpu.set_regs(&regs)?;

    Ok(())
}

fn write_pte(mem: &GuestMemoryMmap, table_addr: u64, index: u64, value: u64) -> Result<(), Error> {
    mem.write_obj(value, GuestAddress(table_addr + index * 8))
        .map_err(|_| Error::GuestMemory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vm_memory::GuestMemoryMmap;

    fn test_mem(size: u64) -> GuestMemoryMmap {
        GuestMemoryMmap::from_ranges(&[(GuestAddress(0), size as usize)])
            .expect("anonymous test guest memory")
    }

    /// The GDT/segment round trip: pack a descriptor, decode it back, and
    /// check every bit we rely on for the 64-bit code segment KVM needs.
    /// This is the single riskiest piece of hand-written bit-twiddling in
    /// the boot path (see README's "what to check first"), so it gets
    /// the most scrutiny here.
    #[test]
    fn gdt_code64_segment_round_trips() {
        let raw = gdt_entry(0xa09b, 0, 0xfffff);
        let seg = kvm_segment_from_gdt(raw, 1);

        assert_eq!(seg.base, 0, "flat segment must have base 0");
        assert_eq!(seg.selector, 1 << 3, "selector = index << 3");
        assert_eq!(seg.present, 1, "P bit");
        assert_eq!(seg.dpl, 0, "ring 0");
        assert_eq!(seg.s, 1, "S=1: code/data, not a system descriptor");
        assert_eq!(
            seg.type_, 0xb,
            "access byte low nibble: exec/read, accessed"
        );
        assert_eq!(seg.l, 1, "L=1: 64-bit code segment");
        assert_eq!(seg.db, 0, "D must be 0 when L=1, per the SDM");
        assert_eq!(seg.g, 1, "G=1: limit is in 4 KiB units");
    }

    #[test]
    fn gdt_data_segment_round_trips() {
        let raw = gdt_entry(0xc093, 0, 0xfffff);
        let seg = kvm_segment_from_gdt(raw, 2);

        assert_eq!(seg.selector, 2 << 3);
        assert_eq!(seg.present, 1);
        assert_eq!(seg.s, 1);
        assert_eq!(seg.type_, 0x3, "read/write data, accessed");
        assert_eq!(seg.l, 0, "data segments don't set L");
        assert_eq!(seg.g, 1);
    }

    #[test]
    fn gdt_null_descriptor_is_all_zero() {
        let raw = gdt_entry(0, 0, 0);
        assert_eq!(raw, 0);
    }

    /// A non-zero base/limit round trip, to catch a swapped nibble in the
    /// packing (the classic x86 descriptor bug: base is split across
    /// three non-contiguous byte ranges).
    #[test]
    fn gdt_nonzero_base_and_limit_round_trip() {
        let raw = gdt_entry(0x8093, 0x1234_5600, 0xabcde);
        let seg = kvm_segment_from_gdt(raw, 3);
        assert_eq!(seg.base, 0x1234_5600);
        assert_eq!(seg.limit, 0xabcde);
    }

    #[test]
    fn page_tables_identity_map_first_page() {
        // 1 GiB guest: exercises exactly one PDPTE/PDE table pair.
        let mem = test_mem(1 << 30);
        build_page_tables(&mem, 1 << 30).unwrap();

        let pml4_0: u64 = mem.read_obj(GuestAddress(PML4_START)).unwrap();
        assert_eq!(pml4_0 & 0x03, 0x03, "PML4[0] present+writable");
        assert_eq!(
            pml4_0 & !0xfff,
            PDPTE_START,
            "PML4[0] points at PDPTE table"
        );

        let pdpte_0: u64 = mem.read_obj(GuestAddress(PDPTE_START)).unwrap();
        assert_eq!(
            pdpte_0 & !0xfff,
            PDE_START,
            "PDPTE[0] points at PDE table 0"
        );

        // First 2 MiB page: identity-mapped, present+writable+huge.
        let pde_0: u64 = mem.read_obj(GuestAddress(PDE_START)).unwrap();
        assert_eq!(
            pde_0, 0x83,
            "PDE[0] maps guest phys 0, present+writable+huge"
        );

        // Last entry of the single PDE table: 511 * 2 MiB.
        let pde_511: u64 = mem.read_obj(GuestAddress(PDE_START + 511 * 8)).unwrap();
        assert_eq!(pde_511, (511u64 * (1 << 21)) | 0x83);
    }

    #[test]
    fn page_tables_span_multiple_gib() {
        // 2.5 GiB guest must round up to 3 GiB-worth of PDE tables and
        // populate a second PDPTE entry.
        let mem = test_mem(3 << 30);
        let mem_size = (2 << 30) + (512 << 20); // 2.5 GiB
        build_page_tables(&mem, mem_size).unwrap();

        let pdpte_1: u64 = mem.read_obj(GuestAddress(PDPTE_START + 8)).unwrap();
        let expected_pde_table_1 = PDE_START + 0x1000;
        assert_eq!(pdpte_1 & !0xfff, expected_pde_table_1);

        // First page of the third GiB (guest phys 2 GiB) should be mapped
        // by PDE table 2, entry 0.
        let pde_table_2 = PDE_START + 2 * 0x1000;
        let pde_2_0: u64 = mem.read_obj(GuestAddress(pde_table_2)).unwrap();
        assert_eq!(pde_2_0, (2u64 << 30) | 0x83);
    }

    #[test]
    fn page_tables_reject_oversized_memory() {
        let mem = test_mem(1 << 20);
        let too_big = (MAX_ADDRESSABLE_1G_PAGES + 1) << 30;
        assert!(matches!(
            build_page_tables(&mem, too_big),
            Err(Error::MemoryTooLarge)
        ));
    }
}
