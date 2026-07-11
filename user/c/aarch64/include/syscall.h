#pragma once

#ifndef NULL
#define NULL ((void *)0)
#endif

#define SYS_READ 0
#define SYS_WRITE 1
#define SYS_EXIT 2
#define SYS_OPEN 3
#define SYS_CLOSE 4
#define SYS_FORK 5
#define SYS_EXECVE 6
#define SYS_WAITPID 7
#define SYS_GETDENTS64 8
#define SYS_CHDIR 9
#define SYS_GETCWD 10

#define GENRT_PATH_MAX 4096
#define PATH_MAX GENRT_PATH_MAX

typedef long ssize_t;
typedef unsigned long size_t;
typedef unsigned int mode_t;
typedef int pid_t;

#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR 2
#define O_CREAT 0100
#define O_TRUNC 01000
#define O_APPEND 02000
#define O_DIRECTORY 0200000

#define DT_UNKNOWN 0
#define DT_DIR 4
#define DT_REG 8

#define WIFEXITED(status) (((status) & 0x7f) == 0)
#define WEXITSTATUS(status) (((status) >> 8) & 0xff)

struct genrt_dirent64 {
    unsigned long long d_ino;
    long long d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

static inline long genrt_syscall0(long nr) {
    register long x0 __asm__("x0");
    register long x8 __asm__("x8") = nr;

    __asm__ volatile("svc #0" : "=r"(x0) : "r"(x8) : "memory");
    return x0;
}

static inline long genrt_syscall1(long nr, long a0) {
    register long x0 __asm__("x0") = a0;
    register long x8 __asm__("x8") = nr;

    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x8) : "memory");
    return x0;
}

static inline long genrt_syscall2(long nr, long a0, long a1) {
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x8 __asm__("x8") = nr;

    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x1), "r"(x8) : "memory");
    return x0;
}

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

static inline int genrt_open3(const char *pathname, int flags, mode_t mode) {
    return (int)genrt_syscall3(SYS_OPEN, (long)pathname, flags, (long)mode);
}

#define open(pathname, flags, ...) genrt_open3((pathname), (flags), 0)

static inline ssize_t read(int fd, void *buf, size_t count) {
    return genrt_syscall3(SYS_READ, fd, (long)buf, (long)count);
}

static inline ssize_t write(int fd, const void *buf, size_t len) {
    return genrt_syscall3(SYS_WRITE, fd, (long)buf, (long)len);
}

static inline int close(int fd) {
    return (int)genrt_syscall3(SYS_CLOSE, fd, 0, 0);
}

static inline pid_t fork(void) {
    return (pid_t)genrt_syscall0(SYS_FORK);
}

static inline int execve(const char *path, char *const argv[], char *const envp[]) {
    return (int)genrt_syscall3(SYS_EXECVE, (long)path, (long)argv, (long)envp);
}

static inline pid_t waitpid(pid_t pid, int *status, int options) {
    return (pid_t)genrt_syscall3(SYS_WAITPID, pid, (long)status, options);
}

static inline long getdents64(int fd, void *dirp, size_t count) {
    return genrt_syscall3(SYS_GETDENTS64, fd, (long)dirp, (long)count);
}

static inline int chdir(const char *path) {
    return (int)genrt_syscall1(SYS_CHDIR, (long)path);
}

static inline char *getcwd(char *buf, size_t size) {
    long result = genrt_syscall2(SYS_GETCWD, (long)buf, (long)size);
    return result < 0 ? NULL : buf;
}

__attribute__((noreturn)) static inline void _exit(int code) {
    register long x0 __asm__("x0") = code;
    register long x8 __asm__("x8") = SYS_EXIT;

    __asm__ volatile("svc #0" : : "r"(x0), "r"(x8) : "memory");
    for (;;) {
        __asm__ volatile("wfe" : : : "memory");
    }
}

__attribute__((noreturn)) static inline void exit(int code) {
    _exit(code);
}
