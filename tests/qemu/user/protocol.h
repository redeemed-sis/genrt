#pragma once

#include "artifact_marker.h"
#include "syscall.h"

#define GTRT_PROTOCOL_BUFFER 256

static unsigned long gtrt_sequence = 1;

static inline size_t gtrt_append(char *out, size_t used, const char *text) {
    while (*text != '\0' && used < GTRT_PROTOCOL_BUFFER) {
        out[used++] = *text++;
    }
    return used;
}

static inline size_t gtrt_append_sequence(char *out, size_t used,
                                          unsigned long sequence) {
    unsigned long divisor = 100000;
    while (divisor != 0 && used < GTRT_PROTOCOL_BUFFER) {
        out[used++] = (char)('0' + ((sequence / divisor) % 10));
        divisor /= 10;
    }
    return used;
}

static inline void gtrt_emit(const char *producer, const char *event,
                             const char *subject, const char *detail) {
    char record[GTRT_PROTOCOL_BUFFER];
    size_t used = 0;
    record[used++] = 0x1e;
    used = gtrt_append(record, used, "GTRT/1|");
    used = gtrt_append(record, used, producer);
    used = gtrt_append(record, used, "|");
    used = gtrt_append_sequence(record, used, gtrt_sequence++);
    used = gtrt_append(record, used, "|");
    used = gtrt_append(record, used, event);
    used = gtrt_append(record, used, "|");
    used = gtrt_append(record, used, subject);
    if (detail != NULL) {
        used = gtrt_append(record, used, "|");
        used = gtrt_append(record, used, detail);
    }
    if (used >= GTRT_PROTOCOL_BUFFER) {
        exit(125);
    }
    record[used++] = '\n';
    if (write(1, record, used) != (ssize_t)used) {
        exit(125);
    }
}

static inline void gtrt_ready(const char *producer, const char *suite) {
    gtrt_emit(producer, "READY", suite, NULL);
}

static inline void gtrt_case_start(const char *producer,
                                   const char *test_case) {
    gtrt_emit(producer, "CASE_START", test_case, NULL);
}

static inline void gtrt_pass(const char *producer, const char *test_case) {
    gtrt_emit(producer, "PASS", test_case, NULL);
}

__attribute__((noreturn)) static inline void
gtrt_fail(const char *producer, const char *test_case, const char *reason) {
    gtrt_emit(producer, "FAIL", test_case, reason);
    exit(1);
}

__attribute__((noreturn)) static inline void gtrt_done(const char *producer,
                                                       const char *suite) {
    gtrt_emit(producer, "DONE", suite, "PASS");
    exit(0);
}
