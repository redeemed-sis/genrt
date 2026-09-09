#include <string.h>

#include <stdint.h>

size_t strlen(const char *text) {
    size_t length = 0;
    while (text[length] != '\0') {
        length++;
    }
    return length;
}

int strcmp(const char *lhs, const char *rhs) {
    while (*lhs == *rhs && *lhs != '\0') {
        lhs++;
        rhs++;
    }
    return (int)(unsigned char)*lhs - (int)(unsigned char)*rhs;
}

size_t strnlen(const char *text, size_t count) {
    size_t length = 0;
    while (length < count && text[length] != '\0') {
        length++;
    }
    return length;
}

int strncmp(const char *lhs, const char *rhs, size_t count) {
    while (count != 0 && *lhs == *rhs && *lhs != '\0') {
        lhs++;
        rhs++;
        count--;
    }
    if (count == 0) {
        return 0;
    }
    return (int)(unsigned char)*lhs - (int)(unsigned char)*rhs;
}

char *strchr(const char *text, int character) {
    const char target = (char)character;
    do {
        if (*text == target) {
            return (char *)text;
        }
    } while (*text++ != '\0');
    return NULL;
}

void *memcpy(void *destination, const void *source, size_t count) {
    unsigned char *out = destination;
    const unsigned char *in = source;
    for (size_t index = 0; index < count; index++) {
        out[index] = in[index];
    }
    return destination;
}

void *memmove(void *destination, const void *source, size_t count) {
    unsigned char *out = destination;
    const unsigned char *in = source;
    if ((uintptr_t)out < (uintptr_t)in) {
        for (size_t index = 0; index < count; index++) {
            out[index] = in[index];
        }
    } else if ((uintptr_t)out > (uintptr_t)in) {
        while (count != 0) {
            count--;
            out[count] = in[count];
        }
    }
    return destination;
}

int memcmp(const void *lhs, const void *rhs, size_t count) {
    const unsigned char *left = lhs;
    const unsigned char *right = rhs;
    for (size_t index = 0; index < count; index++) {
        if (left[index] != right[index]) {
            return (int)left[index] - (int)right[index];
        }
    }
    return 0;
}

void *memset(void *destination, int value, size_t count) {
    unsigned char *out = destination;
    for (size_t index = 0; index < count; index++) {
        out[index] = (unsigned char)value;
    }
    return destination;
}
