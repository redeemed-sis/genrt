#include <unistd.h>

#include "syscall.h"

ssize_t write(int fd, const void *buf, size_t count) {
    return genrt_syscall3(SYS_WRITE, fd, (long)buf, (long)count);
}
