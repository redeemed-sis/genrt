#pragma once

#include "syscall.h"

#define RB_AUTOBOOT 0x01234567
#define RB_POWER_OFF 0x4321fedc

// Linux raw reboot magic is deliberately private to this libc-like wrapper.
// Applications select an operation and never construct the four-argument ABI.
#define GENRT_REBOOT_MAGIC1 0xfee1dead
#define GENRT_REBOOT_MAGIC2 672274793

static inline int reboot(int operation) {
    return (int)genrt_syscall4(SYS_REBOOT,
                               GENRT_REBOOT_MAGIC1,
                               GENRT_REBOOT_MAGIC2,
                               operation,
                               0);
}

#undef GENRT_REBOOT_MAGIC1
#undef GENRT_REBOOT_MAGIC2
