#pragma once

#include <sys/types.h>

#define O_RDONLY 0
#define SYS_MOUNT 23
#define open init_test_open
#define read init_test_read
#define close init_test_close
#define execve init_test_execve
#define dprintf init_test_dprintf
int init_test_open(const char *, int);
ssize_t init_test_read(int, void *, size_t);
int init_test_close(int);
int init_test_execve(const char *, char *const [], char *const []);
int init_test_dprintf(int, const char *, ...);
long genrt_syscall5(long, long, long, long, long, long);
