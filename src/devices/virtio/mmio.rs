//! virtio-mmio (version 2) transport.
//!
//! This is the register file a guest driver pokes at over MMIO. It owns
//! per-queue configuration (size/ready/addresses) and dispatches
//! `QueueNotify` writes into the device backend. No PCI, no discovery
//! mechanism: the guest is told each device's base address and IRQ line
//! directly on the kernel command line
//! (`virtio_mmio.device=4K@0xd0000000:5`), which is how Firecracker and
//! other minimal VMMs avoid needing ACPI/PCI at all.

use std::sync::Arc;

use kvm_ioctls::VmFd;
use virtio_queue::{Queue, QueueT};
use vm_memory::GuestMemoryMmap;

use super::VirtioDevice;

pub const MMIO_MAGIC: u32 = 0x7472_6976; // "virt"
pub const MMIO_VERSION: u32 = 2;
pub const VENDOR_ID: u32 = 0x4d4d_564d; // "MVMM"

const STATUS_ACKNOWLEDGE: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FEATURES_OK: u32 = 8;

/// Everything `MmioTransport` needs from the VM beyond guest memory: the
/// ability to pulse a legacy IRQ line. Split out from a concrete `VmFd`
/// so the transport (and therefore the virtio-mmio protocol logic) is
/// testable in plain userspace, with no `/dev/kvm` -- see `tests/`.
pub trait IrqLine: Send + Sync {
    fn pulse(&self, irq: u32);
}

impl IrqLine for VmFd {
    fn pulse(&self, irq: u32) {
        let _ = self.set_irq_line(irq, true);
        let _ = self.set_irq_line(irq, false);
    }
}

pub struct MmioTransport {
    device: Box<dyn VirtioDevice>,
    queues: Vec<Queue>,
    queue_sel: usize,
    device_features_sel: u32,
    driver_features: u64,
    driver_features_sel: u32,
    status: u32,
    isr: u32,
    irq_raiser: Arc<dyn IrqLine>,
    irq: u32,
}

impl MmioTransport {
    pub fn new(device: Box<dyn VirtioDevice>, irq_raiser: Arc<dyn IrqLine>, irq: u32) -> Self {
        let n = device.num_queues();
        let max_size = device.queue_max_size();
        let queues = (0..n)
            .map(|_| Queue::new(max_size).expect("valid queue max_size"))
            .collect();
        MmioTransport {
            device,
            queues,
            queue_sel: 0,
            device_features_sel: 0,
            driver_features: 0,
            driver_features_sel: 0,
            status: 0,
            isr: 0,
            irq_raiser,
            irq,
        }
    }

    /// Test/introspection accessor -- lets integration tests assert on
    /// device-level state (e.g. `features()`) without needing a second
    /// route through the register file.
    pub fn device_ref(&self) -> &dyn VirtioDevice {
        self.device.as_ref()
    }

    fn cur_queue(&mut self) -> Option<&mut Queue> {
        self.queues.get_mut(self.queue_sel)
    }

    pub fn read(&mut self, offset: u64, data: &mut [u8]) {
        if offset >= 0x100 {
            self.device.read_config(offset - 0x100, data);
            return;
        }
        if data.len() != 4 {
            return; // all core registers are 32-bit; ignore malformed access
        }
        let val: u32 = match offset {
            0x000 => MMIO_MAGIC,
            0x004 => MMIO_VERSION,
            0x008 => self.device.device_type(),
            0x00c => VENDOR_ID,
            0x010 => {
                let f = self.device.features();
                if self.device_features_sel == 0 {
                    f as u32
                } else {
                    (f >> 32) as u32
                }
            }
            0x034 => self
                .queues
                .get(self.queue_sel)
                .map(|q| q.max_size() as u32)
                .unwrap_or(0),
            0x044 => self
                .queues
                .get(self.queue_sel)
                .map(|q| q.ready() as u32)
                .unwrap_or(0),
            0x060 => self.isr,
            0x070 => self.status,
            0x0fc => 0, // config generation: config space is static here
            _ => 0,
        };
        data.copy_from_slice(&val.to_le_bytes());
    }

    pub fn write(&mut self, offset: u64, data: &[u8], mem: &GuestMemoryMmap) {
        if offset >= 0x100 {
            self.device.write_config(offset - 0x100, data);
            return;
        }
        if data.len() != 4 {
            return;
        }
        let val = u32::from_le_bytes(data.try_into().unwrap());
        match offset {
            0x014 => self.device_features_sel = val,
            0x020 => {
                if self.driver_features_sel == 0 {
                    self.driver_features = (self.driver_features & !0xffff_ffff) | val as u64;
                } else {
                    self.driver_features =
                        (self.driver_features & 0xffff_ffff) | ((val as u64) << 32);
                }
            }
            0x024 => self.driver_features_sel = val,
            0x030 => self.queue_sel = val as usize,
            0x038 => {
                if let Some(q) = self.cur_queue() {
                    q.set_size(val as u16);
                }
            }
            0x044 => {
                if let Some(q) = self.cur_queue() {
                    q.set_ready(val != 0);
                }
            }
            0x050 => {
                let idx = val as u16;
                let notify = if let Some(q) = self.queues.get_mut(idx as usize) {
                    if q.ready() {
                        self.device.process_queue(idx, mem, q)
                    } else {
                        false
                    }
                } else {
                    false
                };
                if notify {
                    self.isr |= 0x1;
                    self.irq_raiser.pulse(self.irq);
                }
            }
            0x064 => self.isr &= !val,
            0x070 => {
                if val == 0 {
                    // Device reset.
                    self.status = 0;
                    self.isr = 0;
                    for q in &mut self.queues {
                        q.reset();
                    }
                } else {
                    if val & STATUS_FEATURES_OK != 0 && self.status & STATUS_FEATURES_OK == 0 {
                        self.device.ack_features(self.driver_features);
                    }
                    self.status = val;
                    let _ = (STATUS_ACKNOWLEDGE, STATUS_DRIVER, STATUS_DRIVER_OK);
                    // documented, not gated on
                }
            }
            0x080 => {
                if let Some(q) = self.cur_queue() {
                    q.set_desc_table_address(Some(val), None);
                }
            }
            0x084 => {
                if let Some(q) = self.cur_queue() {
                    q.set_desc_table_address(None, Some(val));
                }
            }
            0x090 => {
                if let Some(q) = self.cur_queue() {
                    q.set_avail_ring_address(Some(val), None);
                }
            }
            0x094 => {
                if let Some(q) = self.cur_queue() {
                    q.set_avail_ring_address(None, Some(val));
                }
            }
            0x0a0 => {
                if let Some(q) = self.cur_queue() {
                    q.set_used_ring_address(Some(val), None);
                }
            }
            0x0a4 => {
                if let Some(q) = self.cur_queue() {
                    q.set_used_ring_address(None, Some(val));
                }
            }
            _ => {}
        }
    }

    /// Re-poll every queue for available work without a guest notification
    /// -- used for virtio-net RX, which is driven by packets arriving on
    /// the TAP fd rather than by the guest.
    pub fn poll_queue(&mut self, idx: u16, mem: &GuestMemoryMmap) {
        let notify = if let Some(q) = self.queues.get_mut(idx as usize) {
            if q.ready() {
                self.device.process_queue(idx, mem, q)
            } else {
                false
            }
        } else {
            false
        };
        if notify {
            self.isr |= 0x1;
            self.irq_raiser.pulse(self.irq);
        }
    }
}
