#pragma once

#include <stddef.h>

/* All pointers must designate valid storage for the documented byte range. */
size_t strlen(const char *text);
/* Return the length before NUL, or count if no NUL occurs in that range. */
size_t strnlen(const char *text, size_t count);
int strcmp(const char *lhs, const char *rhs);
int strncmp(const char *lhs, const char *rhs, size_t count);
char *strchr(const char *text, int character);
void *memcpy(void *destination, const void *source, size_t count);
void *memmove(void *destination, const void *source, size_t count);
int memcmp(const void *lhs, const void *rhs, size_t count);
void *memset(void *destination, int value, size_t count);
