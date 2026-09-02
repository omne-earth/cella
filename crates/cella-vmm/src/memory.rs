//! Guest RAM.
//!
//! We back guest memory with a single `MAP_SHARED` file. This does two jobs
//! at once: it's the memory KVM runs the guest against, and it *is* the
//! on-disk freeze image for RAM — freezing is `msync` + exit, thawing is
//! `mmap` the same file again. No separate RAM dump/restore pass.
//!
//! Guest memory is guest-controlled input. We never form a `&[u8]`/`&mut
//! [u8]` over it — the guest can mutate it concurrently from another vCPU,
//! which would be undefined behaviour over a Rust reference. All access
//! goes through `vm-memory`'s volatile accessors.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use vm_memory::{GuestAddress, GuestMemory, GuestMemoryMmap, GuestMemoryRegion};

pub const GUEST_PHYS_START: u64 = 0x0;

/// Open (creating if needed) the RAM file, size it, and map it MAP_SHARED
/// at guest physical address 0.
///
/// `create` distinguishes a fresh boot (truncate/zero the file) from a
/// thaw (the file already holds the frozen guest image and must not be
/// touched before `vm-memory` maps it).
pub fn open_ram_file(path: &Path, size: u64, create: bool) -> io::Result<(File, GuestMemoryMmap)> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(path)?;

    if create {
        file.set_len(size)?;
    } else {
        let actual = file.metadata()?.len();
        if actual != size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("ram file size {actual} != expected guest memory size {size}"),
            ));
        }
    }

    // vm-memory's mmap backend takes ownership of an fd via FileOffset and
    // maps MAP_SHARED, which is exactly the "RAM is the freeze image"
    // property we want: writes the guest makes go straight to the file.
    let file_offset = vm_memory::FileOffset::new(file.try_clone()?, 0);
    let region = vm_memory::mmap::MmapRegion::from_file(file_offset, size as usize)
        .map_err(|e| io::Error::other(format!("mmap region: {e}")))?;
    let guest_region = vm_memory::GuestRegionMmap::new(region, GuestAddress(GUEST_PHYS_START))
        .map_err(|e| io::Error::other(format!("guest region: {e}")))?;
    let mem = GuestMemoryMmap::from_regions(vec![guest_region])
        .map_err(|e| io::Error::other(format!("guest memory: {e}")))?;

    Ok((file, mem))
}

/// Flush guest RAM to disk. Called during freeze, before we write the
/// state sidecar, so that if we crash between the two, the sidecar's
/// absence (see freeze.rs) correctly marks the image as not-yet-valid.
pub fn sync_ram(mem: &GuestMemoryMmap) -> io::Result<()> {
    for region in mem.iter() {
        // SAFETY: `as_ptr` is valid for `len()` bytes for the lifetime of
        // the mapping, which outlives this call.
        let ret = unsafe {
            libc::msync(
                region.as_ptr() as *mut libc::c_void,
                region.len() as usize,
                libc::MS_SYNC,
            )
        };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Hygiene: keep guest RAM out of core dumps and (best-effort) out of
/// swap. Cheap, and worth doing even though RAM itself isn't encrypted.
pub fn harden_ram(mem: &GuestMemoryMmap) {
    for region in mem.iter() {
        unsafe {
            libc::madvise(
                region.as_ptr() as *mut libc::c_void,
                region.len() as usize,
                libc::MADV_DONTDUMP,
            );
            // Best-effort; a sandbox may not have CAP_IPC_LOCK / sufficient
            // RLIMIT_MEMLOCK. We do not treat failure as fatal.
            libc::mlock(
                region.as_ptr() as *const libc::c_void,
                region.len() as usize,
            );
        }
    }
}
