//! Architecture-neutral terminal system power control.

use core::convert::Infallible;

use crate::errno;

const LINUX_REBOOT_MAGIC1: u32 = 0xfee1_dead;
const LINUX_REBOOT_MAGIC2: u32 = 672_274_793;
const LINUX_REBOOT_MAGIC2A: u32 = 85_072_278;
const LINUX_REBOOT_MAGIC2B: u32 = 369_367_448;
const LINUX_REBOOT_MAGIC2C: u32 = 537_993_216;
const LINUX_REBOOT_CMD_RESTART: u32 = 0x0123_4567;
const LINUX_REBOOT_CMD_POWER_OFF: u32 = 0x4321_fedc;

// Narrow C ABI returned only when a terminal architecture hook comes back.
// Architecture code maps its private firmware statuses to these values.
const ARCH_POWER_NOT_SUPPORTED: i64 = 2;
const ARCH_POWER_DENIED: i64 = 3;

unsafe extern "C" {
    fn arch_system_restart() -> i64;
    fn arch_system_power_off() -> i64;
}

/// Terminal power operation understood by the architecture boundary.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum PowerOperation {
    /// Reset the complete system and begin a new boot.
    Restart,
    /// Remove power from the complete system.
    PowerOff,
}

/// Validate the Linux raw `reboot(2)` command tuple.
///
/// # Arguments
///
/// * `magic` - Linux `LINUX_REBOOT_MAGIC1` value from the first argument.
/// * `magic2` - One accepted Linux secondary magic value.
/// * `command` - Linux reboot command value.
///
/// # Returns
///
/// Returns the architecture-neutral operation for restart or power off.
///
/// # Errors
///
/// Returns [`errno::EINVAL`] when either magic value is invalid or the command
/// is outside the supported restart/power-off subset.
///
/// This function is allocation-free, does not block, and does not change IRQ
/// state. Permission checks may be inserted after validation without changing
/// the userspace ABI when genrt gains a credentials model.
pub(crate) const fn parse_linux_reboot(
    magic: usize,
    magic2: usize,
    command: usize,
) -> Result<PowerOperation, errno::Errno> {
    // Linux declares these raw syscall parameters as 32-bit integer types.
    // AArch64 still transports them in x0..x2, so compare the low UAPI width.
    let magic = magic as u32;
    let magic2 = magic2 as u32;
    let command = command as u32;
    if magic != LINUX_REBOOT_MAGIC1
        || !matches!(
            magic2,
            LINUX_REBOOT_MAGIC2
                | LINUX_REBOOT_MAGIC2A
                | LINUX_REBOOT_MAGIC2B
                | LINUX_REBOOT_MAGIC2C
        )
    {
        return Err(errno::EINVAL);
    }

    match command {
        LINUX_REBOOT_CMD_RESTART => Ok(PowerOperation::Restart),
        LINUX_REBOOT_CMD_POWER_OFF => Ok(PowerOperation::PowerOff),
        _ => Err(errno::EINVAL),
    }
}

/// Request a terminal system power operation from the architecture backend.
///
/// # Arguments
///
/// * `operation` - Validated system-wide restart or power-off request.
///
/// # Returns
///
/// A successful architecture operation never returns. If the backend returns
/// for any reason, this function reports a controlled error.
///
/// # Errors
///
/// Returns [`errno::EIO`] when the architecture backend rejects the request or
/// unexpectedly returns after accepting it.
///
/// The call is allocation-free and does not acquire kernel locks. It may be
/// invoked in syscall thread context on any online CPU and does not alter IRQ
/// state before crossing the architecture boundary.
pub(crate) fn request(operation: PowerOperation) -> Result<Infallible, errno::Errno> {
    let status = unsafe {
        match operation {
            PowerOperation::Restart => arch_system_restart(),
            PowerOperation::PowerOff => arch_system_power_off(),
        }
    };
    Err(backend_errno(status))
}

const fn backend_errno(status: i64) -> errno::Errno {
    match status {
        ARCH_POWER_NOT_SUPPORTED => errno::ENOTSUP,
        ARCH_POWER_DENIED => errno::EPERM,
        _ => errno::EIO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn raw(value: u32) -> usize {
        value as usize
    }

    #[test]
    fn accepts_linux_restart_and_poweroff_commands() {
        for magic2 in [
            LINUX_REBOOT_MAGIC2,
            LINUX_REBOOT_MAGIC2A,
            LINUX_REBOOT_MAGIC2B,
            LINUX_REBOOT_MAGIC2C,
        ] {
            assert_eq!(
                parse_linux_reboot(
                    raw(LINUX_REBOOT_MAGIC1),
                    raw(magic2),
                    raw(LINUX_REBOOT_CMD_RESTART)
                ),
                Ok(PowerOperation::Restart)
            );
            assert_eq!(
                parse_linux_reboot(
                    raw(LINUX_REBOOT_MAGIC1),
                    raw(magic2),
                    raw(LINUX_REBOOT_CMD_POWER_OFF)
                ),
                Ok(PowerOperation::PowerOff)
            );
        }
    }

    #[test]
    fn rejects_invalid_magic_and_unsupported_commands() {
        assert_eq!(
            parse_linux_reboot(0, raw(LINUX_REBOOT_MAGIC2), raw(LINUX_REBOOT_CMD_RESTART)),
            Err(errno::EINVAL)
        );
        assert_eq!(
            parse_linux_reboot(raw(LINUX_REBOOT_MAGIC1), 0, raw(LINUX_REBOOT_CMD_RESTART)),
            Err(errno::EINVAL)
        );
        assert_eq!(
            parse_linux_reboot(raw(LINUX_REBOOT_MAGIC1), raw(LINUX_REBOOT_MAGIC2), 0),
            Err(errno::EINVAL)
        );
    }

    #[test]
    fn maps_architecture_rejections_to_errno() {
        assert_eq!(backend_errno(ARCH_POWER_NOT_SUPPORTED), errno::ENOTSUP);
        assert_eq!(backend_errno(ARCH_POWER_DENIED), errno::EPERM);
        assert_eq!(backend_errno(0), errno::EIO);
        assert_eq!(backend_errno(99), errno::EIO);
    }
}
