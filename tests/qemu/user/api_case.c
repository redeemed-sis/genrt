#include "artifact_marker.h"
#include "affinity.h"
#include "syscall.h"
#include <time.h>
#include <unistd.h>
#include "support_case.h"

#define DIRENT64_HEADER_SIZE 19
#define TASKSET_LARGE_ARG_BYTES 62000
#define LINUX_REBOOT_MAGIC1 0xfee1dead
#define LINUX_REBOOT_MAGIC2 672274793
#define LINUX_REBOOT_CMD_RESTART 0x01234567

#include <string.h>

static int file_io(void) {
    static const char expected[] = "fixture-content-41\n";
    char data[sizeof(expected)];
    int fd = open("/.__genrt_test__/fixtures/known-content", O_RDONLY);
    if (fd < 0) {
        return 1;
    }
    ssize_t count = read(fd, data, sizeof(data));
    if (count != (ssize_t)(sizeof(expected) - 1)
        || memcmp(data, expected, sizeof(expected) - 1) != 0) {
        return 2;
    }
    if (read(fd, data, sizeof(data)) != 0 || close(fd) != 0) {
        return 3;
    }
    return open("/.__genrt_test__/fixtures/missing", O_RDONLY) < 0 ? 0 : 4;
}

static int directory_io(void) {
    int fd = open("/.__genrt_test__/fixtures/directory", O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        return 1;
    }
    unsigned found = 0;
    char buffer[256] __attribute__((aligned(8)));
    for (;;) {
        long count = getdents64(fd, buffer, sizeof(buffer));
        if (count < 0) {
            return 2;
        }
        if (count == 0) {
            break;
        }
        size_t offset = 0;
        while (offset < (size_t)count) {
            struct genrt_dirent64 *entry =
                (struct genrt_dirent64 *)(buffer + offset);
            if (entry->d_reclen < DIRENT64_HEADER_SIZE
                || offset + entry->d_reclen > (size_t)count) {
                return 3;
            }
            unsigned bit = (strcmp(entry->d_name, "a") == 0)
                               ? 1u
                               : ((strcmp(entry->d_name, "b") == 0) ? 2u : 0u);
            if (bit == 0 || (found & bit) != 0) {
                return 4;
            }
            found |= bit;
            offset += entry->d_reclen;
        }
    }
    close(fd);
    return found == 3u ? 0 : 5;
}

static int cwd_paths(void) {
    char cwd[64];
    if (chdir("/.__genrt_test__/fixtures/directory") != 0
        || getcwd(cwd, sizeof(cwd)) == NULL
        || (strcmp(cwd, "/.__genrt_test__/fixtures/directory") != 0)) {
        return 1;
    }
    int fd = open("a", O_RDONLY);
    if (fd < 0 || close(fd) != 0) {
        return 2;
    }
    if (open("/missing/../.__genrt_test__/fixtures/known-content", O_RDONLY) >= 0) {
        return 3;
    }
    if (chdir("/.__genrt_test__/fixtures/known-content/..") == 0) {
        return 4;
    }
    return 0;
}

static int process_control(void) {
    pid_t child = fork();
    if (child < 0) {
        return 1;
    }
    if (child == 0) {
        char *argv[] = {"echo", "api-child", NULL};
        execve("/bin/echo", argv, NULL);
        exit(127);
    }
    if (!gtrt_affinity_is_only(child, 0)) {
        return 2;
    }
    long malformed_pid = (1L << 34) | (unsigned int)child;
    cpu_set_t mask;
    CPU_ZERO(&mask);
    if (genrt_syscall3(SYS_SCHED_GETAFFINITY,
                       malformed_pid,
                       sizeof(mask),
                       (long)&mask)
        != -GTRT_ESRCH) {
        return 3;
    }
    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)
        || WEXITSTATUS(status) != 0) {
        return 4;
    }

    CPU_ZERO(&mask);
    return sched_getaffinity(child, sizeof(mask), &mask) == -GTRT_ESRCH ? 0 : 5;
}

static int fork_affinity_validation(void) {
    if (sched_setforkaffinity(1) >= 0) {
        return 1;
    }
    if (sched_setforkaffinity(-2) >= 0) {
        return 2;
    }
    return sched_setforkaffinity(-1) == 0 ? 0 : 3;
}

static int process_affinity_validation(void) {
    struct {
        cpu_set_t mask;
        unsigned long canary;
    } extended;
    cpu_set_t mask;
    CPU_ZERO(&mask);
    CPU_SET(3, &mask);
    if (!CPU_ISSET(3, &mask)) {
        return 1;
    }
    CPU_CLR(3, &mask);
    if (CPU_ISSET(3, &mask) || !gtrt_affinity_is_only(0, 0)) {
        return 2;
    }
    extended.canary = 0x5a5aa5a5UL;
    if (sched_getaffinity(0, sizeof(extended), &extended.mask) != 0
        || extended.canary != 0x5a5aa5a5UL) {
        return 3;
    }
    if (sched_getaffinity(0, sizeof(mask) - 1, &mask) != -GTRT_EINVAL) {
        return 4;
    }
    if (sched_getaffinity(0, sizeof(mask), (cpu_set_t *)1) != -GTRT_EFAULT) {
        return 5;
    }
    if (sched_getaffinity(0x7fffffff, sizeof(mask), &mask) != -GTRT_ESRCH) {
        return 6;
    }
    return 0;
}

static int reboot_validation(void) {
    if (genrt_syscall4(SYS_REBOOT,
                       0,
                       LINUX_REBOOT_MAGIC2,
                       LINUX_REBOOT_CMD_RESTART,
                       0)
        != -GTRT_EINVAL) {
        return 1;
    }
    if (genrt_syscall4(SYS_REBOOT,
                       LINUX_REBOOT_MAGIC1,
                       0,
                       LINUX_REBOOT_CMD_RESTART,
                       0)
        != -GTRT_EINVAL) {
        return 2;
    }
    return genrt_syscall4(SYS_REBOOT,
                          LINUX_REBOOT_MAGIC1,
                          LINUX_REBOOT_MAGIC2,
                          0,
                          0)
                   == -GTRT_EINVAL
               ? 0
               : 3;
}

static int timespec_before(const struct timespec *lhs,
                           const struct timespec *rhs) {
    return lhs->tv_sec < rhs->tv_sec
           || (lhs->tv_sec == rhs->tv_sec && lhs->tv_nsec < rhs->tv_nsec);
}

static struct timespec timespec_add_ns(struct timespec value, long ns) {
    value.tv_nsec += ns;
    if (value.tv_nsec >= 1000000000L) {
        value.tv_sec++;
        value.tv_nsec -= 1000000000L;
    }
    return value;
}

static int time_api(void) {
    struct timespec resolution;
    struct timespec start;
    struct timespec end;
    if (clock_getres(CLOCK_MONOTONIC, &resolution) != 0
        || resolution.tv_sec < 0 || resolution.tv_nsec < 0
        || resolution.tv_nsec >= 1000000000L
        || (resolution.tv_sec == 0 && resolution.tv_nsec == 0)) {
        return 1;
    }
    if (clock_getres(CLOCK_MONOTONIC, NULL) != 0
        || clock_getres(77, &resolution) != -GTRT_EINVAL
        || clock_getres(CLOCK_MONOTONIC, (struct timespec *)1)
               != -GTRT_EFAULT) {
        return 2;
    }
    if (clock_gettime(CLOCK_MONOTONIC, &start) != 0
        || clock_gettime(CLOCK_MONOTONIC, &end) != 0
        || timespec_before(&end, &start) || start.tv_nsec < 0
        || start.tv_nsec >= 1000000000L
        || clock_gettime(77, &end) != -GTRT_EINVAL
        || clock_gettime(CLOCK_MONOTONIC, (struct timespec *)1)
               != -GTRT_EFAULT) {
        return 3;
    }

    const struct timespec relative = {.tv_sec = 0, .tv_nsec = 2000000L};
    if (clock_gettime(CLOCK_MONOTONIC, &start) != 0
        || clock_nanosleep(CLOCK_MONOTONIC, 0, &relative, NULL) != 0
        || clock_gettime(CLOCK_MONOTONIC, &end) != 0) {
        return 4;
    }
    struct timespec minimum = timespec_add_ns(start, relative.tv_nsec);
    if (timespec_before(&end, &minimum)) {
        return 5;
    }

    const struct timespec short_sleep = {.tv_sec = 0, .tv_nsec = 1000000L};
    const struct timespec zero = {.tv_sec = 0, .tv_nsec = 0};
    const struct timespec bad_nsec = {.tv_sec = 0, .tv_nsec = 1000000000L};
    const struct timespec negative = {.tv_sec = -1, .tv_nsec = 0};
    const struct timespec overflow = {
        .tv_sec = 0x7fffffffffffffffL,
        .tv_nsec = 0,
    };
    if (nanosleep(&short_sleep, NULL) != 0 || usleep(1500) != 0
        || sleep(0) != 0
        || clock_nanosleep(CLOCK_MONOTONIC, 0, &zero,
                           (struct timespec *)1)
               != 0
        || clock_nanosleep(CLOCK_MONOTONIC, 2, &zero, NULL)
               != -GTRT_EINVAL
        || clock_nanosleep(CLOCK_MONOTONIC, 0, &bad_nsec, NULL)
               != -GTRT_EINVAL
        || clock_nanosleep(CLOCK_MONOTONIC, 0, &negative, NULL)
               != -GTRT_EINVAL
        || clock_nanosleep(CLOCK_MONOTONIC, 0, &overflow, NULL)
               != -GTRT_EOVERFLOW
        || clock_nanosleep(CLOCK_MONOTONIC, 0,
                           (const struct timespec *)1, NULL)
               != -GTRT_EFAULT) {
        return 6;
    }

    if (clock_gettime(CLOCK_MONOTONIC, &start) != 0) {
        return 7;
    }
    struct timespec past = start;
    if (past.tv_nsec > 0) {
        past.tv_nsec--;
    } else if (past.tv_sec > 0) {
        past.tv_sec--;
        past.tv_nsec = 999999999L;
    }
    if (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &past, NULL) != 0) {
        return 8;
    }

    struct timespec next = start;
    for (int iteration = 0; iteration < 3; iteration++) {
        next = timespec_add_ns(next, 2000000L);
        if (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &next, NULL) != 0
            || clock_gettime(CLOCK_MONOTONIC, &end) != 0
            || timespec_before(&end, &next)) {
            return 9;
        }
    }

    pid_t child = fork();
    if (child < 0) {
        return 10;
    }
    if (child == 0) {
        const struct timespec child_sleep = {
            .tv_sec = 0,
            .tv_nsec = 20000000L,
        };
        exit(nanosleep(&child_sleep, NULL) == 0 ? 0 : 1);
    }
    struct timespec progress_deadline = timespec_add_ns(end, 5000000L);
    do {
        if (clock_gettime(CLOCK_MONOTONIC, &start) != 0) {
            return 11;
        }
    } while (timespec_before(&start, &progress_deadline));
    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)
        || WEXITSTATUS(status) != 0) {
        return 12;
    }

    if (sleep(1) != 0) {
        return 13;
    }
    return 0;
}

static int taskset_probe(int argc, char **argv) {
    if (argc != 4
        || (strcmp(argv[0], "/.__genrt_test__/bin/api-case") != 0)
        || (strcmp(argv[2], "alpha") != 0)
        || (strcmp(argv[3], "beta") != 0)) {
        return 1;
    }
    return gtrt_affinity_is_only(0, 0) ? 0 : 2;
}

static int taskset_large_probe(int argc, char **argv) {
    if (argc != 3
        || (strcmp(argv[0], "/.__genrt_test__/bin/api-case") != 0)) {
        return 1;
    }
    for (size_t index = 0; index < TASKSET_LARGE_ARG_BYTES; index++) {
        if (argv[2][index] != 'x') {
            return 2;
        }
    }
    if (argv[2][TASKSET_LARGE_ARG_BYTES] != '\0') {
        return 3;
    }
    return gtrt_affinity_is_only(0, 0) ? 0 : 4;
}

int main(int argc, char **argv) {
    if (argc >= 2 && (strcmp(argv[1], "taskset-probe") == 0)) {
        return taskset_probe(argc, argv);
    }
    if (argc >= 2 && (strcmp(argv[1], "taskset-large-probe") == 0)) {
        return taskset_large_probe(argc, argv);
    }
    if (argc != 2) {
        return 64;
    }
    if (strcmp(argv[1], "support-library") == 0) {
        int line = support_library();
        if (line != 0) dprintf(STDERR_FILENO, "support library failed at line %d\n", line);
        return line != 0;
    }
    if ((strcmp(argv[1], "file-io") == 0)) {
        return file_io();
    }
    if ((strcmp(argv[1], "directory-io") == 0)) {
        return directory_io();
    }
    if ((strcmp(argv[1], "cwd-paths") == 0)) {
        return cwd_paths();
    }
    if ((strcmp(argv[1], "process-control") == 0)) {
        return process_control();
    }
    if ((strcmp(argv[1], "fork-affinity-validation") == 0)) {
        return fork_affinity_validation();
    }
    if ((strcmp(argv[1], "process-affinity-validation") == 0)) {
        return process_affinity_validation();
    }
    if ((strcmp(argv[1], "reboot-validation") == 0)) {
        return reboot_validation();
    }
    if ((strcmp(argv[1], "time-api") == 0)) {
        return time_api();
    }
    return 65;
}
