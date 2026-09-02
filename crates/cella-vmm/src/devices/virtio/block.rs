//! virtio-blk, synchronous only.
//!
//! Deliberately the simplest possible backend: every request is a
//! `pread64`/`pwrite64` against the backing file, done inline in the
//! queue-notify handler. No io_uring, no request reordering, no
//! multi-queue. This also means there is never anything "in flight" to
//! drain before a freeze -- see freeze.rs.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;

use virtio_queue::{Queue, QueueT};
use vm_memory::{Bytes, GuestMemoryMmap};

use super::{VirtioDevice, VIRTIO_F_VERSION_1};

const SECTOR_SIZE: u64 = 512;

const VIRTIO_BLK_T_IN: u32 = 0; // read
const VIRTIO_BLK_T_OUT: u32 = 1; // write
const VIRTIO_BLK_T_FLUSH: u32 = 4;

const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

const VIRTIO_BLK_F_RO: u64 = 1 << 5;

#[repr(C)]
#[derive(Clone, Copy)]
struct ReqHeader {
    type_: u32,
    _reserved: u32,
    sector: u64,
}
// SAFETY: plain-old-data, no padding-sensitive invariants, any bit
// pattern is valid (matches the virtio_blk_outhdr wire format exactly).
unsafe impl vm_memory::ByteValued for ReqHeader {}

pub struct Block {
    file: File,
    read_only: bool,
    capacity_sectors: u64,
}

impl Block {
    pub fn new(path: &Path, read_only: bool) -> std::io::Result<Self> {
        let file = OpenOptions::new().read(true).write(!read_only).open(path)?;
        let len = file.metadata()?.len();
        Ok(Block {
            file,
            read_only,
            capacity_sectors: len / SECTOR_SIZE,
        })
    }
}

impl VirtioDevice for Block {
    fn device_type(&self) -> u32 {
        super::VIRTIO_ID_BLOCK
    }

    fn num_queues(&self) -> u16 {
        1
    }

    fn features(&self) -> u64 {
        VIRTIO_F_VERSION_1 | if self.read_only { VIRTIO_BLK_F_RO } else { 0 }
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        // struct virtio_blk_config { u64 capacity; ...all-zero rest... }
        data.fill(0);
        if offset == 0 {
            let cap = self.capacity_sectors.to_le_bytes();
            let n = data.len().min(8);
            data[..n].copy_from_slice(&cap[..n]);
        }
    }

    #[allow(clippy::while_let_loop)] // early-continue logic inside the loop body doesn't fit while-let cleanly
    fn process_queue(&mut self, _idx: u16, mem: &GuestMemoryMmap, queue: &mut Queue) -> bool {
        let mut used_any = false;
        loop {
            let Some(mut chain) = queue.pop_descriptor_chain(mem) else {
                break;
            };
            used_any = true;

            let Some(hdr_desc) = chain.next() else {
                continue;
            };
            let mut status = VIRTIO_BLK_S_OK;
            let mut written_len: u32 = 0;
            let head_index = chain.head_index();

            let hdr: ReqHeader = match mem.read_obj(hdr_desc.addr()) {
                Ok(h) => h,
                Err(_) => {
                    status = VIRTIO_BLK_S_IOERR;
                    ReqHeader {
                        type_: u32::MAX,
                        _reserved: 0,
                        sector: 0,
                    }
                }
            };

            let mut status_addr = None;
            let mut offset_bytes = hdr.sector * SECTOR_SIZE;

            for desc in chain.by_ref() {
                let is_last_1byte = desc.len() == 1 && desc.is_write_only();
                if is_last_1byte {
                    status_addr = Some(desc.addr());
                    continue;
                }
                if status != VIRTIO_BLK_S_OK {
                    continue;
                }
                match hdr.type_ {
                    VIRTIO_BLK_T_IN if desc.is_write_only() => {
                        let mut buf = vec![0u8; desc.len() as usize];
                        match self.file.read_at(&mut buf, offset_bytes) {
                            Ok(n) => {
                                if mem.write_slice(&buf[..n], desc.addr()).is_err() {
                                    status = VIRTIO_BLK_S_IOERR;
                                }
                                written_len += n as u32;
                                offset_bytes += n as u64;
                            }
                            Err(_) => status = VIRTIO_BLK_S_IOERR,
                        }
                    }
                    VIRTIO_BLK_T_OUT if !desc.is_write_only() && !self.read_only => {
                        let mut buf = vec![0u8; desc.len() as usize];
                        if mem.read_slice(&mut buf, desc.addr()).is_err() {
                            status = VIRTIO_BLK_S_IOERR;
                            continue;
                        }
                        match self.file.write_at(&buf, offset_bytes) {
                            Ok(n) => offset_bytes += n as u64,
                            Err(_) => status = VIRTIO_BLK_S_IOERR,
                        }
                    }
                    VIRTIO_BLK_T_FLUSH => {
                        let _ = self.file.sync_data();
                    }
                    _ => status = VIRTIO_BLK_S_UNSUPP,
                }
            }

            if let Some(addr) = status_addr {
                let _ = mem.write_obj(status, addr);
            }

            let _ = queue.add_used(mem, head_index, written_len);
        }
        used_any && queue.needs_notification(mem).unwrap_or(true)
    }
}
