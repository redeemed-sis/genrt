#include "artifact_marker.h"
#include "affinity.h"
#include "syscall.h"

#define DIRENT64_HEADER_SIZE 19
#define TASKSET_LARGE_ARG_BYTES 62000
#define LINUX_REBOOT_MAGIC1 0xfee1dead
#define LINUX_REBOOT_MAGIC2 672274793
#define LINUX_REBOOT_CMD_RESTART 0x01234567

static int string_equal(const char *lhs, const char *rhs) {
    size_t index = 0;
    while (lhs[index] != '\0' && rhs[index] != '\0') {
        if (lhs[index] != rhs[index]) {
            return 0;
        }
        index++;
    }
    return lhs[index] == rhs[index];
}

static int bytes_equal(const char *lhs, const char *rhs, size_t len) {
    for (size_t index = 0; index < len; index++) {
        if (lhs[index] != rhs[index]) {
            return 0;
        }
    }
    return 1;
}

static int file_io(void) {
    static const char expected[] = "fixture-content-41\n";
    char data[sizeof(expected)];
    int fd = open("/.__genrt_test__/fixtures/known-content", O_RDONLY);
    if (fd < 0) {
        return 1;
    }
    ssize_t count = read(fd, data, sizeof(data));
    if (count != (ssize_t)(sizeof(expected) - 1)
        || !bytes_equal(data, expected, sizeof(expected) - 1)) {
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
            unsigned bit = string_equal(entry->d_name, "a")
                               ? 1u
                               : (string_equal(entry->d_name, "b") ? 2u : 0u);
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
        || !string_equal(cwd, "/.__genrt_test__/fixtures/directory")) {
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

static int taskset_probe(int argc, char **argv) {
    if (argc != 4
        || !string_equal(argv[0], "/.__genrt_test__/bin/api-case")
        || !string_equal(argv[2], "alpha")
        || !string_equal(argv[3], "beta")) {
        return 1;
    }
    return gtrt_affinity_is_only(0, 0) ? 0 : 2;
}

static int taskset_large_probe(int argc, char **argv) {
    if (argc != 3
        || !string_equal(argv[0], "/.__genrt_test__/bin/api-case")) {
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
    if (argc >= 2 && string_equal(argv[1], "taskset-probe")) {
        return taskset_probe(argc, argv);
    }
    if (argc >= 2 && string_equal(argv[1], "taskset-large-probe")) {
        return taskset_large_probe(argc, argv);
    }
    if (argc != 2) {
        return 64;
    }
    if (string_equal(argv[1], "file-io")) {
        return file_io();
    }
    if (string_equal(argv[1], "directory-io")) {
        return directory_io();
    }
    if (string_equal(argv[1], "cwd-paths")) {
        return cwd_paths();
    }
    if (string_equal(argv[1], "process-control")) {
        return process_control();
    }
    if (string_equal(argv[1], "fork-affinity-validation")) {
        return fork_affinity_validation();
    }
    if (string_equal(argv[1], "process-affinity-validation")) {
        return process_affinity_validation();
    }
    if (string_equal(argv[1], "reboot-validation")) {
        return reboot_validation();
    }
    return 65;
}
