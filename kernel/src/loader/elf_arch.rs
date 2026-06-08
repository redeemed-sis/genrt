#[cfg(target_arch = "aarch64")]
use elf::abi::EM_AARCH64;

use super::elf::ElfLoadError;

#[cfg(target_arch = "aarch64")]
pub(super) fn validate_machine(machine: u16) -> Result<(), ElfLoadError> {
    if machine == EM_AARCH64 {
        Ok(())
    } else {
        Err(ElfLoadError::UnsupportedMachine)
    }
}

#[cfg(not(target_arch = "aarch64"))]
pub(super) fn validate_machine(_machine: u16) -> Result<(), ElfLoadError> {
    Err(ElfLoadError::UnsupportedMachine)
}
