//! Freeze/thaw.
//!
//! Guest RAM is already a file on disk (memory.rs); freezing only has to
//! add the small pieces that live in KVM/CPU state, not in guest memory:
//! vCPU registers, the kvmclock, and a couple of host-environment values
//! we check on thaw so we refuse to resume onto mismatched hardware
//! rather than silently corrupting a running guest.
//!
//! Crash consistency (including across a host reboot) comes from writing
//! to a temp file and renaming over the real one, with fsyncs at each
//! step that matters. A `state` file's *presence* means "this image can
//! be resumed"; renaming it away atomically is what makes an interrupted
//! freeze safe to just retry, and deleting it on thaw is what makes an
//! image resumable exactly once.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use kvm_bindings::kvm_clock_data;

use crate::vcpu::{IrqChipState, VcpuState, SAVED_MSR_COUNT};

const MAGIC: &[u8; 8] = b"MVMMFRZ1";
// The version of the sidecar format. Version 2 added
// MSR_IA32_TSC_DEADLINE to SAVED_MSRS. Version 3 added the irqchip and
// PIT state. Version 4 added the xsave and xcrs blocks. Version 5
// added MSR_IA32_XSS to SAVED_MSRS. Each of these
// changes moves all data that comes after it in the file. Therefore a
// binary must not read a sidecar that has a different version.
// read_state compares the version and refuses the file if it does not
// agree. No sidecar files exist at an older version, but this check is
// what makes that assumption safe.
const FORMAT_VERSION: u32 = 5;

pub struct HostCheck {
    pub tsc_khz: u32,
}

pub struct FrozenState {
    pub mem_size: u64,
    pub tsc_khz: u32,
    pub vcpu: VcpuState,
    pub clock: kvm_clock_data,
    pub irqchip: IrqChipState,
}

#[allow(dead_code)] // fields read via {:?} in error messages, not field access
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    BadMagic,
    UnsupportedVersion(u32),
    TruncatedFile,
    NotFrozen,
    HardwareMismatch { expected_khz: u32, actual_khz: u32 },
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub fn ram_path(dir: &Path) -> PathBuf {
    dir.join("ram.img")
}
fn state_path(dir: &Path) -> PathBuf {
    dir.join("state")
}
fn state_tmp_path(dir: &Path) -> PathBuf {
    dir.join("state.tmp")
}

/// SAFETY note: every field written here is `#[repr(C)] Copy`, so viewing
/// it as bytes is well-defined (padding bytes are indeterminate but we
/// never rely on their value on read-back within the same build). The
/// state format is therefore tied to this binary's kvm-bindings version --
/// see the format-version check in `read_state`.
unsafe fn as_bytes<T: Copy>(v: &T) -> &[u8] {
    std::slice::from_raw_parts((v as *const T).cast::<u8>(), std::mem::size_of::<T>())
}
unsafe fn from_bytes<T: Copy>(bytes: &[u8]) -> T {
    std::ptr::read_unaligned(bytes.as_ptr().cast::<T>())
}

/// Write the frozen-state sidecar. Guest RAM must already have been
/// `msync`'d by the caller (memory::sync_ram) *before* calling this, so
/// that on a mid-freeze crash the invariant "state exists implies RAM is
/// consistent with it" holds -- the file that might not have made it to
/// disk yet is the one we're about to write last.
pub fn write_state(
    dir: &Path,
    mem_size: u64,
    host: &HostCheck,
    vcpu: &VcpuState,
    clock: &kvm_clock_data,
    irqchip: &IrqChipState,
) -> Result<(), Error> {
    fs::create_dir_all(dir)?;
    let tmp = state_tmp_path(dir);
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)?;

    f.write_all(MAGIC)?;
    f.write_all(&FORMAT_VERSION.to_le_bytes())?;
    f.write_all(&mem_size.to_le_bytes())?;
    f.write_all(&host.tsc_khz.to_le_bytes())?;
    // SAFETY: see as_bytes doc comment above; all fields are repr(C) Copy.
    unsafe {
        f.write_all(as_bytes(&vcpu.regs))?;
        f.write_all(as_bytes(&vcpu.sregs))?;
        f.write_all(as_bytes(&vcpu.fpu))?;
        f.write_all(as_bytes(&vcpu.mp_state))?;
        f.write_all(as_bytes(&vcpu.lapic))?;
        f.write_all(as_bytes(&vcpu.events))?;
        f.write_all(as_bytes(&vcpu.xsave_region))?;
        f.write_all(as_bytes(&vcpu.xcrs))?;
        for (index, data) in &vcpu.msrs {
            f.write_all(&index.to_le_bytes())?;
            f.write_all(&0u32.to_le_bytes())?; // pad
            f.write_all(&data.to_le_bytes())?;
        }
        f.write_all(as_bytes(clock))?;
        // Write these blocks at the end of the file. This keeps the
        // offsets of all data above them the same as in version 2.
        f.write_all(as_bytes(&irqchip.pic_master))?;
        f.write_all(as_bytes(&irqchip.pic_slave))?;
        f.write_all(as_bytes(&irqchip.ioapic))?;
        f.write_all(as_bytes(&irqchip.pit))?;
    }
    f.sync_all()?;
    drop(f);

    // Atomic rename, then fsync the directory: this is what makes the
    // *existence* of `state` a crash-safe signal, on this host or after
    // a reboot.
    fs::rename(&tmp, state_path(dir))?;
    let dir_fd = File::open(dir)?;
    // SAFETY: dir_fd is a valid, open fd for the directory's lifetime.
    unsafe {
        libc::fsync(dir_fd.as_raw_fd());
    }
    Ok(())
}

/// Read (but do not delete) the sidecar. Returns `Error::NotFrozen` if
/// there is no frozen image at `dir` -- the caller should treat that as
/// "boot fresh," not as corruption.
pub fn read_state(dir: &Path) -> Result<FrozenState, Error> {
    let path = state_path(dir);
    let mut f = match File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Error::NotFrozen),
        Err(e) => return Err(e.into()),
    };
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;

    let mut cursor = 0usize;
    let mut take = |n: usize| -> Result<&[u8], Error> {
        if cursor + n > buf.len() {
            return Err(Error::TruncatedFile);
        }
        let s = &buf[cursor..cursor + n];
        cursor += n;
        Ok(s)
    };

    if take(8)? != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u32::from_le_bytes(take(4)?.try_into().unwrap());
    if version != FORMAT_VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let mem_size = u64::from_le_bytes(take(8)?.try_into().unwrap());
    let tsc_khz = u32::from_le_bytes(take(4)?.try_into().unwrap());

    // SAFETY: sizes match exactly what write_state wrote, same struct
    // layouts (same crate version, checked indirectly by FORMAT_VERSION
    // being a stand-in for "built from this same source tree").
    let regs = unsafe { from_bytes(take(std::mem::size_of::<kvm_bindings::kvm_regs>())?) };
    let sregs = unsafe { from_bytes(take(std::mem::size_of::<kvm_bindings::kvm_sregs>())?) };
    let fpu = unsafe { from_bytes(take(std::mem::size_of::<kvm_bindings::kvm_fpu>())?) };
    let mp_state = unsafe { from_bytes(take(std::mem::size_of::<kvm_bindings::kvm_mp_state>())?) };
    let lapic = unsafe { from_bytes(take(std::mem::size_of::<kvm_bindings::kvm_lapic_state>())?) };
    let events = unsafe { from_bytes(take(std::mem::size_of::<kvm_bindings::kvm_vcpu_events>())?) };
    let xsave_region = unsafe { from_bytes(take(std::mem::size_of::<[u32; 1024]>())?) };
    let xcrs = unsafe { from_bytes(take(std::mem::size_of::<kvm_bindings::kvm_xcrs>())?) };

    let mut msrs = [(0u32, 0u64); SAVED_MSR_COUNT];
    for slot in &mut msrs {
        let index = u32::from_le_bytes(take(4)?.try_into().unwrap());
        let _pad = take(4)?;
        let data = u64::from_le_bytes(take(8)?.try_into().unwrap());
        *slot = (index, data);
    }

    let clock = unsafe { from_bytes(take(std::mem::size_of::<kvm_clock_data>())?) };

    let irqchip = unsafe {
        let sz = std::mem::size_of::<kvm_bindings::kvm_irqchip>();
        IrqChipState {
            pic_master: from_bytes(take(sz)?),
            pic_slave: from_bytes(take(sz)?),
            ioapic: from_bytes(take(sz)?),
            pit: from_bytes(take(std::mem::size_of::<kvm_bindings::kvm_pit_state2>())?),
        }
    };

    Ok(FrozenState {
        mem_size,
        tsc_khz,
        vcpu: VcpuState {
            regs,
            sregs,
            fpu,
            mp_state,
            lapic,
            events,
            msrs,
            xsave_region,
            xcrs,
        },
        clock,
        irqchip,
    })
}

pub fn check_hardware(frozen: &FrozenState, actual_tsc_khz: u32) -> Result<(), Error> {
    if frozen.tsc_khz != actual_tsc_khz {
        return Err(Error::HardwareMismatch {
            expected_khz: frozen.tsc_khz,
            actual_khz: actual_tsc_khz,
        });
    }
    Ok(())
}

/// One-shot enforcement: call this only after the restored state has been
/// successfully applied to the new vCPU, and before the first KVM_RUN.
/// Once this returns, the image cannot be thawed again -- forking a
/// frozen image is a deliberate `cp -r` of the whole directory before
/// thawing, not a repeatable thaw.
pub fn finalize_thaw(dir: &Path) -> io::Result<()> {
    fs::remove_file(state_path(dir))
}

pub fn is_frozen(dir: &Path) -> bool {
    state_path(dir).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvm_bindings::{
        kvm_fpu, kvm_irqchip, kvm_lapic_state, kvm_mp_state, kvm_pit_state2, kvm_regs, kvm_sregs,
        kvm_vcpu_events, kvm_xcrs,
    };

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cella-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    /// Make an IrqChipState that has known values. The chip ids and the
    /// PIT count let the round-trip test show that these bytes go to the
    /// file and come back correctly. A test that only reads a file of the
    /// correct length does not show this.
    fn sample_irqchip() -> IrqChipState {
        let mut pit = kvm_pit_state2::default();
        pit.channels[0].count = 12345;
        IrqChipState {
            pic_master: kvm_irqchip {
                chip_id: 0,
                ..Default::default()
            },
            pic_slave: kvm_irqchip {
                chip_id: 1,
                ..Default::default()
            },
            ioapic: kvm_irqchip {
                chip_id: 2,
                ..Default::default()
            },
            pit,
        }
    }

    fn sample_vcpu_state() -> VcpuState {
        let mut lapic_regs = [0i8; 1024];
        lapic_regs[0] = 42;
        lapic_regs[1023] = -7;

        let mut msrs = [(0u32, 0u64); SAVED_MSR_COUNT];
        for (i, slot) in msrs.iter_mut().enumerate() {
            *slot = (0x1000 + i as u32, 0xdead_beef_0000_0000 + i as u64);
        }

        VcpuState {
            regs: kvm_regs {
                rax: 0x1111_2222_3333_4444,
                rip: 0x0000_0000_0010_0200,
                rflags: 0x2,
                ..Default::default()
            },
            sregs: kvm_sregs {
                cr3: 0x9000,
                ..Default::default()
            },
            fpu: kvm_fpu::default(),
            mp_state: kvm_mp_state { mp_state: 0 },
            lapic: kvm_lapic_state { regs: lapic_regs },
            events: kvm_vcpu_events::default(),
            msrs,
            xsave_region: [0u32; 1024],
            xcrs: kvm_xcrs::default(),
        }
    }

    /// The whole point of the sidecar format: what freeze writes, thaw
    /// reads back byte-for-byte identical, including a mp_state field
    /// with no `Default` impl and a 1024-byte LAPIC register array.
    #[test]
    fn freeze_thaw_round_trip_is_exact() {
        let dir = tmp_dir("round-trip");
        let vcpu = sample_vcpu_state();
        let clock = kvm_clock_data {
            clock: 0x1234_5678_9abc,
            flags: 0,
            ..Default::default()
        };
        let host = HostCheck { tsc_khz: 2_500_000 };

        write_state(
            &dir,
            256 * 1024 * 1024,
            &host,
            &vcpu,
            &clock,
            &sample_irqchip(),
        )
        .unwrap();
        assert!(is_frozen(&dir));

        let read_back = read_state(&dir).unwrap();
        assert_eq!(read_back.mem_size, 256 * 1024 * 1024);
        assert_eq!(read_back.tsc_khz, 2_500_000);
        assert_eq!(read_back.vcpu.regs, vcpu.regs);
        assert_eq!(read_back.vcpu.sregs, vcpu.sregs);
        assert_eq!(read_back.vcpu.fpu, vcpu.fpu);
        assert_eq!(read_back.vcpu.mp_state, vcpu.mp_state);
        assert_eq!(read_back.vcpu.lapic, vcpu.lapic);
        assert_eq!(read_back.vcpu.events, vcpu.events);
        assert_eq!(read_back.vcpu.msrs, vcpu.msrs);
        assert_eq!(read_back.clock, clock);
        // The irqchip and PIT block comes after the clock. These checks
        // also show that the data above the block did not move.
        assert_eq!(read_back.irqchip.pit.channels[0].count, 12345);
        assert_eq!(read_back.irqchip.pic_slave.chip_id, 1);
        assert_eq!(read_back.irqchip.ioapic.chip_id, 2);

        let _ = fs::remove_dir_all(&dir);
    }

    /// One-shot enforcement: after finalize_thaw, the image can't be read
    /// (thawed) again -- it reports NotFrozen, same as an image that was
    /// never frozen at all.
    #[test]
    fn finalize_thaw_makes_image_unreadable_again() {
        let dir = tmp_dir("one-shot");
        let vcpu = sample_vcpu_state();
        let clock = kvm_clock_data::default();
        write_state(
            &dir,
            64 * 1024 * 1024,
            &HostCheck { tsc_khz: 1 },
            &vcpu,
            &clock,
            &sample_irqchip(),
        )
        .unwrap();

        finalize_thaw(&dir).unwrap();
        assert!(!is_frozen(&dir));
        assert!(matches!(read_state(&dir), Err(Error::NotFrozen)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_state_on_never_frozen_dir_is_not_frozen() {
        let dir = tmp_dir("never-frozen");
        let _ = fs::create_dir_all(&dir);
        assert!(!is_frozen(&dir));
        assert!(matches!(read_state(&dir), Err(Error::NotFrozen)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_magic_is_rejected() {
        let dir = tmp_dir("bad-magic");
        fs::create_dir_all(&dir).unwrap();
        fs::write(state_path(&dir), b"NOTMAGIC" /* + nothing else */).unwrap();
        assert!(matches!(read_state(&dir), Err(Error::BadMagic)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncated_file_is_rejected_not_misread() {
        let dir = tmp_dir("truncated");
        let vcpu = sample_vcpu_state();
        let clock = kvm_clock_data::default();
        write_state(
            &dir,
            64 * 1024 * 1024,
            &HostCheck { tsc_khz: 1 },
            &vcpu,
            &clock,
            &sample_irqchip(),
        )
        .unwrap();

        // Chop the file in half: this must be a clean error, not a
        // panic or a silently-wrong parse.
        let bytes = fs::read(state_path(&dir)).unwrap();
        fs::write(state_path(&dir), &bytes[..bytes.len() / 2]).unwrap();
        assert!(matches!(read_state(&dir), Err(Error::TruncatedFile)));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsupported_format_version_is_rejected() {
        let dir = tmp_dir("bad-version");
        fs::create_dir_all(&dir).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&999u32.to_le_bytes()); // not FORMAT_VERSION
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        fs::write(state_path(&dir), &bytes).unwrap();
        assert!(matches!(
            read_state(&dir),
            Err(Error::UnsupportedVersion(999))
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hardware_check_refuses_mismatched_tsc() {
        let frozen = FrozenState {
            mem_size: 1,
            tsc_khz: 2_500_000,
            vcpu: sample_vcpu_state(),
            clock: kvm_clock_data::default(),
            irqchip: sample_irqchip(),
        };
        assert!(check_hardware(&frozen, 2_500_000).is_ok());
        assert!(matches!(
            check_hardware(&frozen, 3_000_000),
            Err(Error::HardwareMismatch {
                expected_khz: 2_500_000,
                actual_khz: 3_000_000
            })
        ));
    }
}
