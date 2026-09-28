//! Compile the actual boot backend on the host with strict, thread-local MMIO.
//! This checks register selection and polling, not physical electrical behavior.

extern crate self as kernel;


mod mmu {
    pub fn phys_to_hva(address: usize) -> usize {
        address
    }
}

mod console {
    pub fn on_stdin_byte(byte: u8) {
        super::REGISTERS.with_borrow_mut(|r| r.received.push(byte));
    }
}

use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
};

#[path = "../../arch/aarch64/src/platform/uart/pl011.rs"]
mod pl011;
#[path = "../../arch/aarch64/src/platform/uart/bcm2837.rs"]
mod uart;

fn ranges() -> uart::UartMmio {
    uart::UartMmio {
        gpio: 0x3f20_0000,
        pl011: 0x3f20_1000,
        aux: 0x3f21_5000,
        mini: 0x3f21_5040,
    }
}

#[derive(Default)]
struct Registers {
    values: HashMap<usize, VecDeque<u32>>,
    writes: Vec<(usize, u32)>,
    reads: usize,
    received: Vec<u8>,
}

thread_local! {
    static REGISTERS: RefCell<Registers> = RefCell::new(Registers::default());
}

mod mmio {
    use super::*;

    pub unsafe fn mmio_read32(address: usize) -> u32 {
        REGISTERS.with_borrow_mut(|registers| {
            registers.reads += 1;
            assert!(registers.reads < 100_128, "polling did not terminate");
            let sequence = registers
                .values
                .get_mut(&address)
                .unwrap_or_else(|| panic!("unexpected read at {address:#x}"));
            if sequence.len() > 1 {
                sequence.pop_front().unwrap()
            } else {
                sequence[0]
            }
        })
    }

    pub unsafe fn mmio_write32(address: usize, value: u32) {
        REGISTERS.with_borrow_mut(|registers| registers.writes.push((address, value)));
    }
}

fn set(address: usize, values: &[u32]) {
    REGISTERS.with_borrow_mut(|r| {
        r.values.insert(address, values.iter().copied().collect());
    });
}

fn reset() {
    REGISTERS.with_borrow_mut(|r| *r = Registers::default());
}

fn mini() {
    reset();
    set(0x3f200004, &[(2 << 12) | (2 << 15)]);
    set(0x3f215004, &[1]);
    set(0x3f215060, &[3]);
    set(0x3f21504c, &[3]);
}

fn pl011() {
    reset();
    set(0x3f200004, &[(4 << 12) | (4 << 15)]);
    set(0x3f201030, &[0x301]);
    set(0x3f20102c, &[0x70]);
}

fn no_writes() {
    REGISTERS.with_borrow(|r| assert!(r.writes.is_empty()));
}

#[test]
fn mini_inherits_configuration_and_polls_before_write_and_drain() {
    mini();
    let port = unsafe { uart::initialize(&ranges()) }.unwrap();
    assert!(matches!(port, uart::Port::Mini(_)));
    no_writes();
    set(0x3f215054, &[0, 0x20, 0x20, 0x60]);
    unsafe {
        port.putc(b'M');
        port.drain();
    }
    REGISTERS.with_borrow(|r| assert_eq!(r.writes, [(0x3f215040, u32::from(b'M'))]));
}

#[test]
fn pl011_inherits_configuration_and_polls_before_write_and_drain() {
    pl011();
    let port = unsafe { uart::initialize(&ranges()) }.unwrap();
    assert!(matches!(port, uart::Port::Pl011(_)));
    no_writes();
    set(0x3f201018, &[0x20, 0, 8, 0]);
    unsafe {
        port.putc(b'P');
        port.drain();
    }
    REGISTERS.with_borrow(|r| assert_eq!(r.writes, [(0x3f201000, u32::from(b'P'))]));
}

#[test]
fn unsupported_pinmux_and_disabled_aux_do_not_touch_uart() {
    for pins in [0, 4 << 12, (2 << 12) | (4 << 15)] {
        reset();
        set(0x3f200004, &[pins]);
        assert!(unsafe { uart::initialize(&ranges()) }.is_none());
        no_writes();
    }
    reset();
    set(0x3f200004, &[(2 << 12) | (2 << 15)]);
    set(0x3f215004, &[0]);
    assert!(unsafe { uart::initialize(&ranges()) }.is_none());
    no_writes();
}

#[test]
fn mini_rejects_disabled_transmitter_flow_control_and_dlab() {
    for (address, value) in [
        (0x3f215060, 1),
        (0x3f215060, 11),
        (0x3f21504c, 0x83),
        (0x3f21504c, 0),
    ] {
        mini();
        set(address, &[value]);
        assert!(unsafe { uart::initialize(&ranges()) }.is_none());
        no_writes();
    }
}

#[test]
fn pl011_rejects_disabled_transmitter_flow_control_and_non_8n1() {
    for (address, value) in [
        (0x3f201030, 1),
        (0x3f201030, 0x300),
        (0x3f201030, 0x8301),
        (0x3f20102c, 0x71),
        (0x3f20102c, 0x72),
        (0x3f20102c, 0x78),
        (0x3f20102c, 0x50),
    ] {
        pl011();
        set(address, &[value]);
        assert!(unsafe { uart::initialize(&ranges()) }.is_none());
        no_writes();
    }
}

#[test]
fn uart_register_accesses_follow_supplied_bases() {
    let shifted = uart::UartMmio {
        gpio: 0x3e20_0000,
        pl011: 0x3e20_1000,
        aux: 0x3e21_5000,
        mini: 0x3e21_5040,
    };
    reset();
    set(shifted.gpio + 4, &[(4 << 12) | (4 << 15)]);
    set(shifted.pl011 + 0x30, &[0x301]);
    set(shifted.pl011 + 0x2c, &[0x70]);
    let pl011 = unsafe { uart::initialize(&shifted) }.unwrap();
    set(shifted.pl011 + 0x18, &[0]);
    unsafe { pl011.putc(b'P') };
    REGISTERS.with_borrow(|r| {
        assert_eq!(r.writes, [(shifted.pl011, u32::from(b'P'))]);
    });

    reset();
    set(shifted.gpio + 4, &[(2 << 12) | (2 << 15)]);
    set(shifted.aux + 4, &[1]);
    set(shifted.mini + 0x20, &[3]);
    set(shifted.mini + 0x0c, &[3]);
    let mini = unsafe { uart::initialize(&shifted) }.unwrap();
    set(shifted.mini + 0x14, &[0x20]);
    unsafe { mini.putc(b'M') };
    REGISTERS.with_borrow(|r| {
        assert_eq!(r.writes, [(shifted.mini, u32::from(b'M'))]);
    });
}

// Runtime selection is process-global, just as it is in the kernel. Exercise
// both choices in one test so concurrent pure adoption tests cannot race it.
#[test]
fn runtime_routes_receives_and_bounds_output_for_both_ports() {
    for (setup, irq, status, data, ready) in [
        (mini as fn(), 29, 0x3f215054, 0x3f215040, 1),
        (pl011 as fn(), 57, 0x3f201018, 0x3f201000, 0),
    ] {
        setup();
        uart::runtime::configure_from_ranges(ranges());
        uart::runtime::init_once();
        assert_eq!(uart::runtime::gpu_irq(), irq);
        uart::runtime::enable_rx_interrupts();
        let empty = if irq == 29 { 0 } else { 1 << 4 };
        set(status, &[ready, ready, empty]);
        set(data, &[b'a' as u32, b'\n' as u32]);
        uart::runtime::on_uart_irq();
        REGISTERS.with_borrow(|r| assert_eq!(r.received, b"a\n"));

        // A permanently asserted RX source must not trap the CPU in one IRQ.
        set(status, &[ready]);
        uart::runtime::on_uart_irq();
        REGISTERS.with_borrow(|r| assert_eq!(r.received.len(), 66));

        REGISTERS.with_borrow_mut(|r| {
            r.reads = 0;
            r.writes.clear();
        });
        set(status, &[if irq == 29 { 0 } else { 1 << 5 }]);
        uart::runtime::putc_raw(b'x');
        REGISTERS.with_borrow(|r| {
            assert_eq!(r.reads, 100_000);
            assert!(r.writes.is_empty());
        });
    }
}
