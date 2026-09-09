#pragma once

#include <stdarg.h>
#include <stddef.h>

#include <genrt/errno.h>

/*
 * Format into buffer using s, c, d, i, u, x, X, p, and % conversions with
 * optional zero padding, width, and l, ll, or z modifiers. snprintf and
 * vsnprintf return the required byte count excluding the trailing NUL, write
 * a NUL when size is nonzero, and support buffer == NULL only when size is
 * zero. Malformed conversions return a negative GENRT_E* result.
 */
int vsnprintf(char *buffer, size_t size, const char *format, va_list arguments);
int snprintf(char *buffer, size_t size, const char *format, ...);

/*
 * Format using bounded 256-byte staging and write to fd (or stdout for printf).
 * Short writes are retried. Return total bytes on success, a negative backend
 * errno on failure, or -GENRT_EIO if a write makes no progress. An error may
 * follow a written prefix. No complete-message atomicity or heap allocation.
 * All functions return -GENRT_EOVERFLOW if the result cannot fit in int.
 */
int vdprintf(int fd, const char *format, va_list arguments);
int dprintf(int fd, const char *format, ...);
int vprintf(const char *format, va_list arguments);
int printf(const char *format, ...);
