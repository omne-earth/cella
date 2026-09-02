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

/// The device-side position of one queue. The rings live in guest RAM,
/// which the freeze preserves; these fields are the private view of the
/// device, and the sidecar must carry them (see docs/DEVICE-STATE.md).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct QueueState {
    pub ready: bool,
    pub size: u16,
    pub desc_table: u64,
    pub avail_ring: u64,
    pub used_ring: u64,
    pub next_avail: u16,
    pub next_used: u16,
}

/// Everything a thaw must put back into a fresh MmioTransport. The
/// held egress frames are frames read from the TX ring and not yet
/// written to the TAP at the freeze instant (see docs/DEVICE-STATE.md).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TransportState {
    pub status: u32,
    pub queue_sel: u32,
    pub isr: u32,
    pub driver_features: u64,
    pub queues: Vec<QueueState>,
    /// Parked egress frames: the descriptor head index, and the frame
    /// bytes (vnet header included).
    pub held_frames: Vec<(u16, Vec<u8>)>,
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

    /// Copy the device-side state out, for the freeze sidecar,
    /// parked egress frames included.
    pub fn save_state(&self) -> TransportState {
        TransportState {
            status: self.status,
            queue_sel: self.queue_sel as u32,
            isr: self.isr,
            driver_features: self.driver_features,
            queues: self
                .queues
                .iter()
                .map(|q| QueueState {
                    ready: q.ready(),
                    size: q.size(),
                    desc_table: q.desc_table(),
                    avail_ring: q.avail_ring(),
                    used_ring: q.used_ring(),
                    next_avail: q.next_avail(),
                    next_used: q.next_used(),
                })
                .collect(),
            held_frames: self.device.held_frames(),
        }
    }

    /// Put the device-side state back into a fresh transport, at thaw.
    /// The guest driver keeps its own copy in RAM, and the two sides
    /// must agree before the first KVM_RUN. The feature bits go to the
    /// device backend again: the guest negotiated them once, before
    /// the freeze, and does not negotiate again.
    pub fn restore_state(
        &mut self,
        st: &TransportState,
        open_ops: &[crate::ledger::OpenOperation],
    ) {
        if st.status & STATUS_FEATURES_OK != 0 {
            self.device.ack_features(st.driver_features);
        }
        self.status = st.status;
        self.queue_sel = st.queue_sel as usize;
        self.isr = st.isr;
        self.driver_features = st.driver_features;
        for (q, qs) in self.queues.iter_mut().zip(st.queues.iter()) {
            q.set_size(qs.size);
            q.set_desc_table_address(
                Some(qs.desc_table as u32),
                Some((qs.desc_table >> 32) as u32),
            );
            q.set_avail_ring_address(
                Some(qs.avail_ring as u32),
                Some((qs.avail_ring >> 32) as u32),
            );
            q.set_used_ring_address(Some(qs.used_ring as u32), Some((qs.used_ring >> 32) as u32));
            q.set_next_avail(qs.next_avail);
            q.set_next_used(qs.next_used);
            q.set_ready(qs.ready);
        }
        self.device.restore_held(st.held_frames.clone(), open_ops);
    }

    /// Set the valve posture (see docs/NETWORK-MODEL.md).
    pub fn set_valve(&mut self, v: super::ValveState) {
        self.device.set_valve(v);
    }

    /// Drain the device's pending ledger events, for the chronicle.
    pub fn drain_ledger_events(&mut self) -> Vec<crate::proto::Event> {
        self.device.drain_ledger_events()
    }

    /// True when any frame parked since the last take (joins
    /// included) -- the freeze trigger of the one-shot rule.
    pub fn take_parked_flag(&mut self) -> bool {
        self.device.take_parked_flag()
    }

    /// Apply a decision map to the device's held operations, oldest-
    /// parked first (see docs/NETWORK-MODEL.md, "Release names an
    /// id"): every operation the map lets resolve right now
    /// delivers its frames to the TAP and completes them -- the
    /// buffer is marked used and the interrupt is raised, because at
    /// the park instant the guest owned no completion, and without
    /// this step its driver would leak the descriptor. A refusal
    /// resolves silently, with no completion: its frames never
    /// reach the guest's used ring, the same as a request that
    /// never sent.
    pub fn apply_decisions(
        &mut self,
        decisions: &std::collections::HashMap<Vec<u8>, crate::proto::Decision>,
        mem: &GuestMemoryMmap,
    ) {
        let (frames, refused) = self.device.resolve_decisions(decisions);
        if frames.is_empty() && refused.is_empty() {
            return;
        }
        let qidx = self.device.egress_queue() as usize;
        for (head, frame) in &frames {
            self.device.write_egress(frame);
            if let Some(q) = self.queues.get_mut(qidx) {
                let _ = q.add_used(mem, *head, 0);
            }
        }
        // A refused frame dies, and its buffer returns: the guest
        // posted the descriptor, and without a completion its
        // driver watchdog declares the queue dead.
        for head in &refused {
            if let Some(q) = self.queues.get_mut(qidx) {
                let _ = q.add_used(mem, *head, 0);
            }
        }
        self.isr |= 0x1;
        self.irq_raiser.pulse(self.irq);
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
