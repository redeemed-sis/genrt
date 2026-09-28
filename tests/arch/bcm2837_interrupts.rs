//! Register-level controller checks; CPU barriers and firmware are not emulated.

extern crate self as kernel;

use std::cell::RefCell;
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        panic!($($arg)*)
    };
}


#[derive(Default)]
pub struct Range {
    start: usize,
    size: usize,
}
#[derive(Default)]
pub struct BootPlatformInfo {
    uart: Range,
    irq_primary: Range,
    irq_secondary: Range,
}
pub struct PlatformInfo;

#[derive(Debug, PartialEq)]
enum Event {
    Write(usize, u32),
    EnableRx,
    Rx,
    Timer,
    Checkpoint,
}
thread_local! {
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
    static PENDING: RefCell<u32> = const { RefCell::new(0) };
    static UART_IRQ: RefCell<u32> = const { RefCell::new(57) };
    static CPU: RefCell<usize> = const { RefCell::new(0) };
}
fn record(event: Event) {
    EVENTS.with_borrow_mut(|events| events.push(event));
}
mod mmio {
    pub unsafe fn mmio_read32(pa: usize) -> u32 {
        if pa == 0x3f100020 {
            return 0x800;
        }
        if pa == 0x3f10001c {
            return 0x110;
        }
        assert_eq!(pa, 0x40000060 + super::arch_current_cpu_hardware_id() * 4);
        super::PENDING.with_borrow(|value| *value)
    }
    pub unsafe fn mmio_write32(pa: usize, value: u32) {
        super::record(super::Event::Write(pa, value));
    }
}
mod mmu {
    pub fn phys_to_hva(pa: usize) -> usize {
        pa
    }
}
pub mod arch {
    pub struct ActiveContext<'a>(pub &'a mut ());
}
pub mod sched {
    pub fn finish_scheduler_ipi(_: &mut crate::arch::ActiveContext<'_>) {
        crate::record(crate::Event::Checkpoint);
    }
}
mod timer {
    pub fn on_timer_irq(_: &mut crate::arch::ActiveContext<'_>) {
        crate::record(crate::Event::Timer);
    }
}
mod console {
    pub fn enable_rx_interrupts() {
        crate::record(crate::Event::EnableRx);
    }
    pub fn on_uart_irq() {
        crate::record(crate::Event::Rx);
    }
}
mod uart {
    pub mod bcm2837 {
        pub mod runtime {
            pub fn gpu_irq() -> u32 {
                crate::UART_IRQ.with_borrow(|irq| *irq)
            }
        }
    }
}
fn arch_current_cpu_hardware_id() -> usize {
    CPU.with_borrow(|value| *value)
}

mod platform {
    pub(crate) use crate::controller as interrupt_controller;
    pub(crate) use crate::PlatformInfo;
}

#[path = "../../arch/aarch64/src/interrupts.rs"]
mod interrupts;

#[path = "../../arch/aarch64/src/platform/raspberrypi.rs"]
mod controller;
#[path = "../../arch/aarch64/src/platform/pi_power.rs"]
mod power;

fn reset_power_registers() {
    EVENTS.with_borrow_mut(Vec::clear);
}
fn assert_power_writes(expected: &[(usize, u32)]) {
    EVENTS.with_borrow(|events| {
        assert_eq!(
            *events,
            expected
                .iter()
                .map(|&(pa, value)| Event::Write(pa, value))
                .collect::<Vec<_>>()
        )
    });
}

#[test]
fn routes_only_selected_uart_and_cpu0_local_sources() {
    for (irq, enable, bit) in [(29, 0x3f00b210, 29), (57, 0x3f00b214, 25)] {
        EVENTS.with_borrow_mut(Vec::clear);
        UART_IRQ.with_borrow_mut(|value| *value = irq);
        interrupts::init_global(&PlatformInfo);
        assert!(interrupts::init_local());
        EVENTS.with_borrow(|events| {
            assert_eq!(
                events,
                &[
                    Event::Write(0x3f00b21c, u32::MAX),
                    Event::Write(0x3f00b220, u32::MAX),
                    Event::Write(0x3f00b224, u32::MAX),
                    Event::Write(0x3f00b20c, 0),
                    Event::Write(0x4000000c, 0),
                    Event::Write(0x40000000, 0),
                    Event::Write(0x40000008, 0x80000000),
                    Event::EnableRx,
                    Event::Write(enable, 1 << bit),
                    Event::Write(0x40000040, 2),
                    Event::Write(0x400000c0, u32::MAX),
                    Event::Write(0x40000050, 1),
                ]
            )
        });
    }
}

#[test]
fn mailbox_is_cleared_before_one_return_checkpoint() {
    for (pending, expected) in [
        (0, vec![]),
        (16, vec![Event::Write(0x400000c0, 1), Event::Checkpoint]),
        (2, vec![Event::Timer]),
        (256 | 16 | 2, vec![Event::Rx, Event::Timer]),
    ] {
        EVENTS.with_borrow_mut(Vec::clear);
        PENDING.with_borrow_mut(|value| *value = pending);
        interrupts::dispatch(&mut arch::ActiveContext(&mut ()));
        EVENTS.with_borrow(|events| assert_eq!(*events, expected));
    }
}

#[test]
fn cpu_binding_and_notification_reject_secondary_targets() {
    assert!(interrupts::bind_cpu(0));
    assert!(!interrupts::bind_cpu(1));
    assert!(!interrupts::send_ipi(4));
    EVENTS.with_borrow(|events| assert!(events.is_empty()));
    assert!(interrupts::send_ipi(0));
    EVENTS.with_borrow(|events| assert_eq!(*events, [Event::Write(0x40000080, 1)]));
    let mut ranges = BootPlatformInfo::default();
    ranges.uart = Range { start: 0x3f000000, size: 0x1000000 };
    controller::boot_ranges(&mut ranges);
    assert_eq!(ranges.uart.start, 0x3f000000);
    assert_eq!(ranges.uart.size, 0x1000000);
    assert_eq!(ranges.irq_primary.start, 0x40000000);
    assert_eq!(ranges.irq_primary.size, 0x1000);
    assert_eq!(ranges.irq_secondary.size, 0);
}

#[test]
fn secondary_registers_and_mailboxes_are_owner_local() {
    for cpu in 1..4 {
        CPU.with_borrow_mut(|value| *value = cpu);
        EVENTS.with_borrow_mut(Vec::clear);
        assert!(interrupts::bind_cpu(cpu));
        assert!(!interrupts::bind_cpu(0));
        assert!(interrupts::init_local());
        assert!(interrupts::send_ipi(cpu));
        PENDING.with_borrow_mut(|value| *value = 16 | 256);
        interrupts::dispatch(&mut arch::ActiveContext(&mut ()));
        EVENTS.with_borrow(|events| {
            assert_eq!(
                *events,
                [
                    Event::Write(0x40000040 + cpu * 4, 2),
                    Event::Write(0x400000c0 + cpu * 16, u32::MAX),
                    Event::Write(0x40000050 + cpu * 4, 1),
                    Event::Write(0x40000080 + cpu * 16, 1),
                    Event::Write(0x400000c0 + cpu * 16, 1),
                    Event::Checkpoint,
                ]
            )
        });
    }
}
