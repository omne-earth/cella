//! Serial console: vm-superio's 16550 model, hooked to stdout and to a
//! `Trigger` that pulses legacy IRQ4 through KVM's in-kernel PIC.

use std::io;
use std::sync::Arc;

use kvm_ioctls::VmFd;
use vm_superio::serial::NoEvents;
use vm_superio::{Serial, Trigger};

const SERIAL_IRQ: u32 = 4;

pub struct IrqTrigger {
    vm: Arc<VmFd>,
}

impl Trigger for IrqTrigger {
    type E = io::Error;
    fn trigger(&self) -> Result<(), io::Error> {
        let _ = self.vm.set_irq_line(SERIAL_IRQ, true);
        let _ = self.vm.set_irq_line(SERIAL_IRQ, false);
        Ok(())
    }
}

pub struct SerialDevice {
    inner: Serial<IrqTrigger, NoEvents, io::Stdout>,
}

impl SerialDevice {
    pub fn new(vm: Arc<VmFd>) -> Self {
        SerialDevice {
            inner: Serial::new(IrqTrigger { vm }, io::stdout()),
        }
    }

    pub fn read(&mut self, port: u16) -> u8 {
        self.inner.read((port - 0x3f8) as u8)
    }

    /// The nine 16550 registers, for the freeze sidecar. The RX FIFO
    /// is not saved: a byte in flight at the freeze instant is lost,
    /// and a terminal retype replaces it.
    pub fn registers(&self) -> [u8; 9] {
        let st = self.inner.state();
        [
            st.baud_divisor_low,
            st.baud_divisor_high,
            st.interrupt_enable,
            st.interrupt_identification,
            st.line_control,
            st.line_status,
            st.modem_control,
            st.modem_status,
            st.scratch,
        ]
    }

    /// Rebuild the device from frozen registers. Without this, a thaw
    /// gives the guest a reset UART: the interrupt-enable register
    /// reads 0, RX bytes raise no IRQ, and a shell on the serial line
    /// never hears another keystroke.
    pub fn restore(vm: Arc<VmFd>, regs: [u8; 9]) -> Self {
        let state = vm_superio::serial::SerialState {
            baud_divisor_low: regs[0],
            baud_divisor_high: regs[1],
            interrupt_enable: regs[2],
            interrupt_identification: regs[3],
            line_control: regs[4],
            line_status: regs[5],
            modem_control: regs[6],
            modem_status: regs[7],
            scratch: regs[8],
            in_buffer: Vec::new(),
        };
        SerialDevice {
            inner: Serial::from_state(&state, IrqTrigger { vm }, NoEvents, io::stdout())
                .expect("an empty in_buffer cannot overflow the FIFO"),
        }
    }

    /// Feed host input into the RX FIFO of the guest. vm-superio raises
    /// IRQ4 when the guest has RX interrupts enabled. A full FIFO drops
    /// the rest of the bytes; the caller polls stdin once per run-loop
    /// pass, thus the loss window is small and a terminal retype fixes
    /// it.
    pub fn enqueue(&mut self, bytes: &[u8]) {
        let _ = self.inner.enqueue_raw_bytes(bytes);
    }

    pub fn write(&mut self, port: u16, val: u8) {
        let _ = self.inner.write((port - 0x3f8) as u8, val);
    }
}
