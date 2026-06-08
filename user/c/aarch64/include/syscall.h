#pragma once

#define SYS_WRITE 1
#define SYS_EXIT 2

static inline long genrt_syscall3(long nr, long a0, long a1, long a2) {
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x8 __asm__("x8") = nr;

    __asm__ volatile("svc #0"
                     : "+r"(x0)
                     : "r"(x1), "r"(x2), "r"(x8)
                     : "memory");
    return x0;
}

static inline long write(int fd, const void *buf, unsigned long len) {
    return genrt_syscall3(SYS_WRITE, fd, (long)buf, (long)len);
}

__attribute__((noreturn)) static inline void exit(int code) {
    register long x0 __asm__("x0") = code;
    register long x8 __asm__("x8") = SYS_EXIT;

    __asm__ volatile("svc #0" : : "r"(x0), "r"(x8) : "memory");
    for (;;) {
        __asm__ volatile("wfe" : : : "memory");
    }
}
