#pragma once

#include "artifact_marker.h"
#include "syscall.h"
#include <unistd.h>
#include <stdio.h>

#define GTRT_PROTOCOL_BUFFER 256

static unsigned long gtrt_sequence = 1;

static inline void gtrt_emit(const char *producer, const char *event,
                             const char *subject, const char *detail) {
    char record[GTRT_PROTOCOL_BUFFER];
    if (gtrt_sequence > 999999) {
        exit(125);
    }
    int used = snprintf(record, sizeof(record), "\036GTRT/1|%s|%06lu|%s|%s%s%s\n",
                        producer, gtrt_sequence++, event, subject,
                        detail != NULL ? "|" : "", detail != NULL ? detail : "");
    // Keep protocol records whole; fd formatting may split long messages.
    if (used < 0 || (size_t)used >= sizeof(record)
        || write(STDOUT_FILENO, record, (size_t)used) != (ssize_t)used) {
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

static inline void gtrt_terminal(const char *producer, const char *test_case,
                                 const char *operation) {
    gtrt_emit(producer, "TERMINAL", test_case, operation);
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
