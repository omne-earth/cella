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

    pub fn write(&mut self, port: u16, val: u8) {
        let _ = self.inner.write((port - 0x3f8) as u8, val);
    }
}
