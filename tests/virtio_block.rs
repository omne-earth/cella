//! Exercises `devices::virtio::block::Block` the same way `MmioTransport`
//! does on a real `QueueNotify` -- real guest memory (anonymous mmap, no
//! KVM involved), a real `virtio_queue::Queue` populated via
//! `virtio_queue::mock`, and a real backing file. This is the "test the
//! feature across the compiled product" counterpart to the from-scratch
//! GDT/page-table unit tests: it runs the actual descriptor-chain-walking
//! code path, not a re-description of it.

use std::io::Write;

use cella::devices::virtio::block::Block;
use cella::devices::virtio::VirtioDevice;

use virtio_bindings::bindings::virtio_ring::VRING_DESC_F_WRITE;
use virtio_queue::mock::MockSplitQueue;
use virtio_queue::{Descriptor, Queue};
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

const HDR_ADDR: u64 = 0x10_0000;
const DATA_ADDR: u64 = 0x10_1000;
const STATUS_ADDR: u64 = 0x10_2000;

fn test_mem() -> GuestMemoryMmap {
    GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 4 * 1024 * 1024)])
        .expect("anonymous test guest memory")
}

fn write_header(mem: &GuestMemoryMmap, req_type: u32, sector: u64) {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&req_type.to_le_bytes());
    bytes[8..16].copy_from_slice(&sector.to_le_bytes());
    mem.write_slice(&bytes, GuestAddress(HDR_ADDR)).unwrap();
}

fn temp_disk(name: &str, contents: &[u8]) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("cella-test-disk-{}-{name}", std::process::id()));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(contents).unwrap();
    f.sync_all().unwrap();
    path
}

/// Build one descriptor chain (header, data, status), push it to the
/// avail ring, and hand back a `Queue` configured exactly the way
/// `MmioTransport` would configure it after processing the guest's
/// register writes.
fn setup_queue(mem: &GuestMemoryMmap, data_len: u32, data_write_only: bool) -> Queue {
    let mq = MockSplitQueue::new(mem, 16);
    let data_flags = if data_write_only {
        VRING_DESC_F_WRITE as u16
    } else {
        0
    };
    let descs = [
        Descriptor::new(HDR_ADDR, 16, 0, 0),
        Descriptor::new(DATA_ADDR, data_len, data_flags, 0),
        Descriptor::new(STATUS_ADDR, 1, VRING_DESC_F_WRITE as u16, 0),
    ];
    let _ = mq.build_desc_chain(&descs).unwrap();
    mq.create_queue::<Queue>().unwrap()
}

#[test]
fn read_request_copies_disk_bytes_into_guest_memory() {
    let mem = test_mem();
    let disk_contents: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    let disk_path = temp_disk("read", &disk_contents);

    write_header(&mem, VIRTIO_BLK_T_IN, 0 /* sector */);
    let mut queue = setup_queue(&mem, 512, true /* device writes into this buffer */);

    let mut block = Block::new(&disk_path, false).unwrap();
    let interrupt = block.process_queue(0, &mem, &mut queue);
    assert!(interrupt, "a completed request should ask for an interrupt");

    let mut got = [0u8; 512];
    mem.read_slice(&mut got, GuestAddress(DATA_ADDR)).unwrap();
    assert_eq!(&got[..], &disk_contents[..512]);

    let status: u8 = mem.read_obj(GuestAddress(STATUS_ADDR)).unwrap();
    assert_eq!(status, VIRTIO_BLK_S_OK);

    let _ = std::fs::remove_file(&disk_path);
}

#[test]
fn write_request_copies_guest_bytes_to_disk() {
    let mem = test_mem();
    let disk_path = temp_disk("write", &[0u8; 4096]);

    let guest_data: Vec<u8> = (0u8..=255).rev().cycle().take(512).collect();
    mem.write_slice(&guest_data, GuestAddress(DATA_ADDR))
        .unwrap();

    write_header(
        &mem,
        VIRTIO_BLK_T_OUT,
        2, /* sector 2 = byte offset 1024 */
    );
    let mut queue = setup_queue(&mem, 512, false /* device reads this buffer */);

    let mut block = Block::new(&disk_path, false).unwrap();
    block.process_queue(0, &mem, &mut queue);

    let on_disk = std::fs::read(&disk_path).unwrap();
    assert_eq!(&on_disk[1024..1536], &guest_data[..]);

    let status: u8 = mem.read_obj(GuestAddress(STATUS_ADDR)).unwrap();
    assert_eq!(status, VIRTIO_BLK_S_OK);

    let _ = std::fs::remove_file(&disk_path);
}

#[test]
fn read_only_device_rejects_write_requests() {
    let mem = test_mem();
    let disk_path = temp_disk("ro-reject", &[0u8; 4096]);
    let original = std::fs::read(&disk_path).unwrap();

    let guest_data = [0xAAu8; 512];
    mem.write_slice(&guest_data, GuestAddress(DATA_ADDR))
        .unwrap();
    write_header(&mem, VIRTIO_BLK_T_OUT, 0);
    let mut queue = setup_queue(&mem, 512, false);

    let mut block = Block::new(&disk_path, true /* read_only */).unwrap();
    block.process_queue(0, &mem, &mut queue);

    // The write must not have landed on disk.
    let after = std::fs::read(&disk_path).unwrap();
    assert_eq!(
        after, original,
        "read-only device must not modify the backing file"
    );

    let _ = std::fs::remove_file(&disk_path);
}

#[test]
fn read_only_feature_bit_is_advertised() {
    let mem = test_mem();
    let disk_path = temp_disk("ro-feature", &[0u8; 512]);
    let _ = &mem; // silence unused-var if the assertion below is all we need

    let ro = Block::new(&disk_path, true).unwrap();
    const VIRTIO_BLK_F_RO: u64 = 1 << 5;
    assert_ne!(ro.features() & VIRTIO_BLK_F_RO, 0);

    let rw = Block::new(&disk_path, false).unwrap();
    assert_eq!(rw.features() & VIRTIO_BLK_F_RO, 0);

    let _ = std::fs::remove_file(&disk_path);
}

#[test]
fn unrecognized_request_type_yields_unsupported_status() {
    let mem = test_mem();
    let disk_path = temp_disk("unsupp", &[0u8; 4096]);

    write_header(&mem, 0xdead_beef /* not IN/OUT/FLUSH */, 0);
    let mut queue = setup_queue(&mem, 512, true);

    let mut block = Block::new(&disk_path, false).unwrap();
    block.process_queue(0, &mem, &mut queue);

    let status: u8 = mem.read_obj(GuestAddress(STATUS_ADDR)).unwrap();
    assert_eq!(status, VIRTIO_BLK_S_UNSUPP);

    let _ = std::fs::remove_file(&disk_path);
}

#[test]
fn capacity_config_matches_file_size_in_sectors() {
    let disk_path = temp_disk("capacity", &vec![0u8; 8 * 512]); // 8 sectors
    let block = Block::new(&disk_path, false).unwrap();

    let mut cap_bytes = [0u8; 8];
    block.read_config(0, &mut cap_bytes);
    assert_eq!(u64::from_le_bytes(cap_bytes), 8);

    let _ = std::fs::remove_file(&disk_path);
}
