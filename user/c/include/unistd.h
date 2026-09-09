#pragma once

#include <getopt.h>
#include <sys/types.h>

/* Standard input, output, and error descriptor numbers. */
#define STDIN_FILENO 0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2

typedef unsigned int useconds_t;

/* Write up to count readable bytes from buf to fd. Return the number written
 * (possibly short or zero), or a direct negative errno. The syscall adapter
 * owns pointer validation and descriptor semantics; this function does not
 * retry partial writes or allocate storage. */
ssize_t write(int fd, const void *buf, size_t count);

/* Sleep for seconds using the monotonic clock. Return zero on success or the
 * requested seconds on failure; remaining-time reporting awaits signals. */
unsigned int sleep(unsigned int seconds);

/* Sleep for usec microseconds. Return zero or a direct negative errno. */
int usleep(useconds_t usec);
