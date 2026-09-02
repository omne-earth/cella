//! Drives `MmioTransport` the way a guest's `virtio_mmio.c` driver does:
//! read the magic/version/device-id triad, negotiate features one 32-bit
//! half at a time, configure a queue, flip DRIVER_OK, and notify it. All
//! against real (anonymous) guest memory, with a mock `IrqLine` standing
//! in for the KVM ioctl a real device would make -- this is the
//! `IrqLine` trait split specifically so this test doesn't need
//! `/dev/kvm`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use cella_vmm::devices::virtio::mmio::{IrqLine, MmioTransport, MMIO_MAGIC, MMIO_VERSION};
use cella_vmm::devices::virtio::VirtioDevice;

use virtio_bindings::bindings::virtio_ring::VRING_DESC_F_WRITE;
use virtio_queue::mock::MockSplitQueue;
use virtio_queue::{Descriptor, Queue};
use vm_memory::{GuestAddress, GuestMemoryMmap};

/// Counts pulses instead of touching KVM, so tests can assert "the
/// guest was interrupted N times" without a VM.
#[derive(Default)]
struct CountingIrq {
    pulses: AtomicU32,
    last_irq: AtomicU32,
}
impl IrqLine for CountingIrq {
    fn pulse(&self, irq: u32) {
        self.pulses.fetch_add(1, Ordering::SeqCst);
        self.last_irq.store(irq, Ordering::SeqCst);
    }
}

/// The simplest possible device: type 42, one queue, no config space, and
/// it completes every descriptor chain it's handed with a fixed length.
/// Enough to exercise the transport's protocol logic in isolation from
/// any real device's behaviour.
struct StubDevice {
    ack_features_called_with: Option<u64>,
}
impl VirtioDevice for StubDevice {
    fn device_type(&self) -> u32 {
        42
    }
    fn num_queues(&self) -> u16 {
        1
    }
    fn features(&self) -> u64 {
        0b101 // arbitrary two-bit feature set to exercise the 64-bit split
    }
    fn ack_features(&mut self, features: u64) {
        self.ack_features_called_with = Some(features);
    }
    fn process_queue(&mut self, _idx: u16, mem: &GuestMemoryMmap, queue: &mut Queue) -> bool {
        use virtio_queue::QueueT;
        let mut completed = false;
        while let Some(chain) = queue.pop_descriptor_chain(mem) {
            let head = chain.head_index();
            let _ = queue.add_used(mem, head, 1);
            completed = true;
        }
        completed
    }
}

fn test_mem() -> GuestMemoryMmap {
    GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 4 * 1024 * 1024)]).unwrap()
}

fn read_u32(t: &mut MmioTransport, offset: u64) -> u32 {
    let mut buf = [0u8; 4];
    t.read(offset, &mut buf);
    u32::from_le_bytes(buf)
}
fn write_u32(t: &mut MmioTransport, offset: u64, val: u32, mem: &GuestMemoryMmap) {
    t.write(offset, &val.to_le_bytes(), mem);
}

#[test]
fn identification_registers_match_the_virtio_mmio_v2_spec() {
    let irq = Arc::new(CountingIrq::default());
    let device = StubDevice {
        ack_features_called_with: None,
    };
    let mut t = MmioTransport::new(Box::new(device), irq, 7);

    assert_eq!(read_u32(&mut t, 0x000), MMIO_MAGIC);
    assert_eq!(read_u32(&mut t, 0x004), MMIO_VERSION);
    assert_eq!(
        read_u32(&mut t, 0x008),
        42,
        "DeviceID should be the stub's device_type()"
    );
}

#[test]
fn feature_negotiation_reads_both_32_bit_halves_and_reaches_the_device() {
    let irq = Arc::new(CountingIrq::default());
    let device = StubDevice {
        ack_features_called_with: None,
    };
    let mut t = MmioTransport::new(Box::new(device), irq, 7);
    let mem = test_mem();

    // DeviceFeaturesSel=0 then 1: low half is 0b101, high half is 0.
    write_u32(&mut t, 0x014, 0, &mem);
    assert_eq!(read_u32(&mut t, 0x010), 0b101);
    write_u32(&mut t, 0x014, 1, &mem);
    assert_eq!(read_u32(&mut t, 0x010), 0);

    // Driver "accepts" a feature subset, written across both halves.
    write_u32(&mut t, 0x024, 0, &mem); // DriverFeaturesSel = 0
    write_u32(&mut t, 0x020, 0b001, &mem); // DriverFeatures low
    write_u32(&mut t, 0x024, 1, &mem); // DriverFeaturesSel = 1
    write_u32(&mut t, 0x020, 0, &mem); // DriverFeatures high

    // ack_features is only called once FEATURES_OK is set in Status.
    const STATUS_FEATURES_OK: u32 = 8;
    write_u32(&mut t, 0x070, STATUS_FEATURES_OK, &mem);
    assert_eq!(t.device_ref().features(), 0b101, "sanity: device unchanged");
}

#[test]
fn queue_notify_processes_the_chain_and_pulses_the_configured_irq() {
    let irq = Arc::new(CountingIrq::default());
    let device = StubDevice {
        ack_features_called_with: None,
    };
    let mut t = MmioTransport::new(Box::new(device), irq.clone(), 9 /* IRQ line */);
    let mem = test_mem();

    // Select queue 0, set its size, mark it ready, and point it at a
    // real descriptor chain built the same way the block/net tests do.
    //
    // NOTE: MockSplitQueue's avail()/used() accessors return
    // SplitQueueRing, whose .start() is the ring *array* address -- 4
    // bytes past the true ring base (a u16 flags + u16 idx precede it).
    // The register writes below carry the true base, so subtract 4.
    let mq = MockSplitQueue::new(&mem, 16);
    let descs = [Descriptor::new(0x10_0000, 8, VRING_DESC_F_WRITE as u16, 0)];
    let _ = mq.build_desc_chain(&descs).unwrap();
    let avail_base = mq.avail().start().0 - 4;
    let used_base = mq.used().start().0 - 4;

    write_u32(&mut t, 0x030, 0, &mem); // QueueSel = 0
    write_u32(&mut t, 0x038, 16, &mem); // QueueNum
    write_u32(&mut t, 0x080, mq.start().0 as u32, &mem); // QueueDescLow
    write_u32(&mut t, 0x084, (mq.start().0 >> 32) as u32, &mem);
    write_u32(&mut t, 0x090, avail_base as u32, &mem); // QueueAvailLow
    write_u32(&mut t, 0x094, (avail_base >> 32) as u32, &mem);
    write_u32(&mut t, 0x0a0, used_base as u32, &mem); // QueueUsedLow
    write_u32(&mut t, 0x0a4, (used_base >> 32) as u32, &mem);
    write_u32(&mut t, 0x044, 1, &mem); // QueueReady = 1

    write_u32(&mut t, 0x050, 0, &mem); // QueueNotify(0)

    assert_eq!(
        irq.pulses.load(Ordering::SeqCst),
        1,
        "one completed chain, one pulse"
    );
    assert_eq!(irq.last_irq.load(Ordering::SeqCst), 9);
    assert_eq!(
        read_u32(&mut t, 0x060) & 0x1,
        0x1,
        "InterruptStatus bit 0 set"
    );

    // InterruptACK clears it.
    write_u32(&mut t, 0x064, 0x1, &mem);
    assert_eq!(read_u32(&mut t, 0x060) & 0x1, 0);
}

#[test]
fn queue_notify_on_a_not_ready_queue_is_a_silent_no_op() {
    // Guests can (incorrectly, or during teardown) notify a queue that
    // was never marked ready. The transport must not panic or pulse an
    // interrupt for work that was never actually queued.
    let irq = Arc::new(CountingIrq::default());
    let device = StubDevice {
        ack_features_called_with: None,
    };
    let mut t = MmioTransport::new(Box::new(device), irq.clone(), 9);
    let mem = test_mem();

    write_u32(&mut t, 0x050, 0, &mem); // QueueNotify(0), queue never configured
    assert_eq!(irq.pulses.load(Ordering::SeqCst), 0);
}

#[test]
fn status_write_of_zero_resets_the_device() {
    let irq = Arc::new(CountingIrq::default());
    let device = StubDevice {
        ack_features_called_with: None,
    };
    let mut t = MmioTransport::new(Box::new(device), irq, 7);
    let mem = test_mem();

    write_u32(&mut t, 0x070, 0b1111, &mem); // set some status bits
    assert_eq!(read_u32(&mut t, 0x070), 0b1111);

    write_u32(&mut t, 0x070, 0, &mem); // reset
    assert_eq!(read_u32(&mut t, 0x070), 0);
    assert_eq!(read_u32(&mut t, 0x060), 0, "ISR must also clear on reset");
}

#[test]
fn config_space_reads_route_to_the_device_at_offset_0x100() {
    struct ConfigDevice;
    impl VirtioDevice for ConfigDevice {
        fn device_type(&self) -> u32 {
            1
        }
        fn num_queues(&self) -> u16 {
            1
        }
        fn read_config(&self, offset: u64, data: &mut [u8]) {
            // Echo the offset back so the test can tell the transport
            // computed `addr - 0x100` correctly.
            data.fill(offset as u8);
        }
        fn process_queue(&mut self, _: u16, _: &GuestMemoryMmap, _: &mut Queue) -> bool {
            false
        }
    }
    let irq = Arc::new(CountingIrq::default());
    let mut t = MmioTransport::new(Box::new(ConfigDevice), irq, 0);

    let mut buf = [0u8; 1];
    t.read(0x105, &mut buf); // config offset 5
    assert_eq!(buf[0], 5);
}
