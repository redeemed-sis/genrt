#pragma once

#include <syscall.h>

#define MS_RDONLY 1UL

/* Global additive mount; no credentials model or unmount yet. Supports only
 * /dev/diskNpM, absolute existing directories, vfat/fat32, 0/MS_RDONLY and NULL
 * data. Returns zero or direct negative errno. Existing cwd/FDs stay pinned. */
static inline int mount(const char *source, const char *target,
                        const char *filesystemtype, unsigned long mountflags,
                        const void *data) {
    return (int)genrt_syscall5(SYS_MOUNT, (long)source, (long)target,
                             (long)filesystemtype, (long)mountflags, (long)data);
}
