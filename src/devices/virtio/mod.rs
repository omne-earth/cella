pub mod block;
pub mod mmio;
pub mod net;
pub mod tap;

use virtio_queue::Queue;
use vm_memory::GuestMemoryMmap;

pub const VIRTIO_ID_NET: u32 = 1;
pub const VIRTIO_ID_BLOCK: u32 = 2;

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
        0 // no VIRTIO_F_VERSION_1 assumed: legacy-ish minimal transport, no offloads
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
}
