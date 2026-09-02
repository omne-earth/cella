pub mod block;
pub mod mmio;
pub mod net;
pub mod tap;

use virtio_queue::Queue;
use vm_memory::GuestMemoryMmap;

pub const VIRTIO_ID_NET: u32 = 1;
pub const VIRTIO_ID_BLOCK: u32 = 2;

/// Mandatory for any device on a "modern" (non-legacy) transport --
/// which virtio-mmio *version 2* (see mmio.rs's MMIO_VERSION) always
/// is. Without this bit, virtio_finalize_features() on the guest side
/// rejects the device outright: it reads device_features once and
/// never writes anything back, so negotiation just silently stops.
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

/// A virtio device backend. The transport (`mmio.rs`) owns queue
/// configuration and register plumbing; this trait is only the
/// device-specific behaviour: feature bits, config space, and processing
/// descriptor chains when a queue is kicked.
pub trait VirtioDevice: Send {
    fn device_type(&self) -> u32;
    fn num_queues(&self) -> u16;
    fn queue_max_size(&self) -> u16 {
        256
    }
    fn features(&self) -> u64 {
        VIRTIO_F_VERSION_1
    }
    fn ack_features(&mut self, _features: u64) {}
    fn read_config(&self, _offset: u64, data: &mut [u8]) {
        data.fill(0);
    }
    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    /// Process descriptors currently available on queue `idx`. Returns
    /// true if the guest should be interrupted (i.e. at least one
    /// descriptor was completed).
    fn process_queue(&mut self, idx: u16, mem: &GuestMemoryMmap, queue: &mut Queue) -> bool;

    /// Egress hold, for the freeze at the egress moment (see
    /// docs/DEVICE-STATE.md). With hold on, an outbound frame parks
    /// at a defined point instead of leaving the machine. Only
    /// virtio-net implements these; the defaults are inert.
    fn set_hold(&mut self, _on: bool) {}
    /// The parked frames, without draining them (the freeze reads
    /// them into the sidecar and then exits).
    fn held_frames(&self) -> Vec<(u16, Vec<u8>)> {
        Vec::new()
    }
    /// Put parked frames back, at thaw, before delivery.
    fn restore_held(&mut self, _frames: Vec<(u16, Vec<u8>)>) {}
    /// Drain the parked frames, for delivery.
    fn take_held(&mut self) -> Vec<(u16, Vec<u8>)> {
        Vec::new()
    }
    /// Write one frame out of the machine, past the park point.
    fn write_egress(&mut self, _frame: &[u8]) {}
    /// The queue whose descriptors the parked frames came from.
    fn egress_queue(&self) -> u16 {
        0
    }
    /// Install a pass entry: frames to this destination flow at full
    /// speed under hold (the allow verdict).
    fn allow(&mut self, _ip: [u8; 4], _port: u16) {}
    /// Ledger events accumulated since the last drain -- one per new
    /// operation parked, for the chronicle (see
    /// docs/NETWORK-MODEL.md, "The control plane"). Only virtio-net
    /// implements this; the default is inert.
    fn drain_ledger_events(&mut self) -> Vec<crate::proto::Event> {
        Vec::new()
    }
}
