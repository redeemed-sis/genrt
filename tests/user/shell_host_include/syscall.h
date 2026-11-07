#pragma once

#include <sys/types.h>
#include <stdio.h>
#include <unistd.h>

#define PATH_MAX 4096
#define O_RDONLY 0
#define O_WRONLY 1
#define O_CREAT 0100
#define O_TRUNC 01000
#define O_APPEND 02000
#define O_DIRECTORY 0200000
#define DT_REG 8
#define DT_DIR 4
#define WIFEXITED(status) (((status) & 0x7f) == 0)

struct genrt_dirent64 {
    unsigned long long d_ino;
    long long d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

#define open shell_test_open
#define read shell_test_read
#define write shell_test_write
#define close shell_test_close
#define dup2 shell_test_dup2
#define fork shell_test_fork
#define execve shell_test_execve
#define waitpid shell_test_waitpid
#define chdir shell_test_chdir
#define getdents64 shell_test_getdents64
#define exit shell_test_exit
#define printf shell_test_printf
#define dprintf shell_test_dprintf

int shell_test_open(const char *path, int flags);
ssize_t shell_test_read(int fd, void *buffer, size_t count);
ssize_t shell_test_write(int fd, const void *buffer, size_t count);
int shell_test_close(int fd);
int shell_test_dup2(int fd, int target);
pid_t shell_test_fork(void);
int shell_test_execve(const char *path, char *const argv[], char *const envp[]);
pid_t shell_test_waitpid(pid_t pid, int *status, int options);
int shell_test_chdir(const char *path);
long shell_test_getdents64(int fd, void *buffer, size_t count);
__attribute__((noreturn)) void shell_test_exit(int code);
int shell_test_printf(const char *format, ...);
int shell_test_dprintf(int fd, const char *format, ...);
