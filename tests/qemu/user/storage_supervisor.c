#include "protocol.h"
#include <string.h>

#define PRODUCER "storage-supervisor"
#define SUITE "storage-contract"
#define PERSIST_BYTES 65573u

static void check(int ok, const char *subject) {
    if (!ok) gtrt_fail(PRODUCER, subject, "ASSERT");
}

static unsigned char pattern(size_t at) { return (unsigned char)(at * 37u + 11u); }

static int write_pattern(int fd, size_t length) {
    static unsigned char bytes[PERSIST_BYTES];
    size_t offset = 0;
    while (offset < length) {
        size_t count = length - offset;
        if (count > sizeof(bytes)) count = sizeof(bytes);
        for (size_t i = 0; i < count; i++) bytes[i] = pattern(offset + i);
        ssize_t done = write(fd, bytes, count);
        if (done <= 0 || (size_t)done > count) return 0;
        offset += (size_t)done;
    }
    return 1;
}

static int read_pattern(int fd, size_t length) {
    static unsigned char bytes[PERSIST_BYTES];
    size_t offset = 0;
    while (offset < length) {
        ssize_t done = read(fd, bytes, sizeof(bytes));
        if (done <= 0 || (size_t)done > sizeof(bytes) || offset + (size_t)done > length) return 0;
        for (size_t i = 0; i < (size_t)done; i++) if (bytes[i] != pattern(offset + i)) return 0;
        offset += (size_t)done;
    }
    return read(fd, bytes, 1) == 0;
}

static void boot_mount_policy(int writing) {
    const char *subject = "boot-mount-policy";
    gtrt_case_start(PRODUCER, subject);
    const char *extra = "/mnt/disk/EXTRA/seed.txt";
    if (writing) {
        check(open("/mnt/disk/fstab", O_RDONLY) == -2, subject);
        check(open(extra, O_RDONLY) == -2, subject);
    } else {
        char bytes[32], cwd[64];
        int fd = open("/mnt/disk/fstab", O_RDONLY);
        check(fd >= 3 && read(fd, bytes, sizeof(bytes)) > 0 && close(fd) == 0, subject);
        fd = open(extra, O_RDONLY);
        check(fd >= 3 && read(fd, bytes, sizeof(bytes)) == 16
              && memcmp(bytes, "second FAT seed\n", 16) == 0, subject);
        check(fsync(fd) == 0 && close(fd) == 0, subject);
        check(open(extra, O_WRONLY) == -30
              && open("/mnt/disk/EXTRA/new.txt", O_CREAT | O_WRONLY) == -30, subject);
        fd = open("/mnt/disk/extra/seed.txt", O_RDONLY);
        check(fd >= 3 && read(fd, bytes, sizeof(bytes)) == 16
              && memcmp(bytes, "second FAT seed\n", 16) == 0 && close(fd) == 0, subject);
        check(open("/mnt/disk/extra/new.txt", O_CREAT | O_WRONLY) == -30, subject);
        check(chdir("/mnt/disk/EXTRA") == 0 && chdir("..") == 0
              && getcwd(cwd, sizeof(cwd)) != NULL && strcmp(cwd, "/mnt/disk") == 0
              && chdir("/") == 0, subject);
        // Separate volumes share one physical controller across real syscalls.
        check(sched_setforkaffinity(2) == 0, subject);
        pid_t child = fork();
        check(child >= 0, subject);
        if (child == 0) {
            int input = open(extra, O_RDONLY);
            if (input < 3) _exit(1);
            for (unsigned i = 0; i < 64; i++) {
                if (read(input, bytes, sizeof(bytes)) != 16
                    || memcmp(bytes, "second FAT seed\n", 16) != 0
                    || close(input) != 0) _exit(2);
                input = open(extra, O_RDONLY);
                if (input < 3) _exit(3);
            }
            _exit(close(input) == 0 ? 0 : 4);
        }
        int output = open("/mnt/disk/MOUNTS.BIN", O_CREAT | O_TRUNC | O_WRONLY);
        check(output >= 3, subject);
        for (unsigned i = 0; i < 64; i++) {
            check(write(output, "mount", 5) == 5 && fsync(output) == 0, subject);
        }
        int status = -1;
        check(close(output) == 0 && waitpid(child, &status, 0) == child
              && WIFEXITED(status) && WEXITSTATUS(status) == 0, subject);
    }
    gtrt_pass(PRODUCER, subject);
}

static void mount_paths(void) {
    const char *subject = "mount-paths";
    gtrt_case_start(PRODUCER, subject);
    int fd = open("/mnt/disk/sub/seed.txt", O_RDONLY);
    char bytes[256] __attribute__((aligned(8))), cwd[64];
    check(fd >= 3 && read(fd, bytes, sizeof(bytes)) == 14 && memcmp(bytes, "host FAT seed\n", 14) == 0, subject);
    check(close(fd) == 0 && chdir("/mnt/disk/sub") == 0, subject);
    fd = open("/mnt/disk/DEEP/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/leaf.txt", O_RDONLY);
    check(fd >= 3 && read(fd, bytes, sizeof(bytes)) == 14 && close(fd) == 0, subject);
    check(chdir("../..") == 0 && getcwd(cwd, sizeof(cwd)) != NULL && strcmp(cwd, "/mnt") == 0, subject);
    check(chdir("/") == 0 && open("/mnt/disk/missing/../SUB", O_RDONLY) == -2, subject);
    fd = open("/mnt/disk", O_RDONLY | O_DIRECTORY);
    check(fd >= 3, subject);
    int found = 0;
    for (unsigned i = 0; i < 32; i++) {
        long count = getdents64(fd, bytes, sizeof(bytes));
        check(count >= 0, subject);
        if (count == 0) break;
        struct genrt_dirent64 *entry = (void *)bytes;
        if (strcmp(entry->d_name, "SUB") == 0) found = 1;
    }
    check(found && close(fd) == 0, subject);
    gtrt_pass(PRODUCER, subject);
}

static void writable_io(void) {
    const char *subject = "writable-io";
    gtrt_case_start(PRODUCER, subject);
    const char *path = "/mnt/disk/Long filename.bin";
    int fd = open(path, O_CREAT | O_TRUNC | O_RDWR);
    check(fd >= 3 && write_pattern(fd, 6001) && fsync(fd) == 0 && close(fd) == 0, subject);
    fd = open(path, O_RDONLY);
    check(fd >= 3 && read_pattern(fd, 6001) && close(fd) == 0, subject);
    int append = open(path, O_WRONLY | O_APPEND);
    int other = open(path, O_WRONLY | O_TRUNC);
    check(append >= 3 && other >= 3 && write(other, "A", 1) == 1 && close(other) == 0, subject);
    check(write(append, "B", 1) == 1 && fsync(append) == 0 && close(append) == 0, subject);
    fd = open(path, O_RDONLY);
    char bytes[3];
    check(fd >= 3 && read(fd, bytes, sizeof(bytes)) == 2 && memcmp(bytes, "AB", 2) == 0, subject);
    check(close(fd) == 0 && fsync(fd) == -9 && fsync(0) == -9 && fsync(999) == -9, subject);
    gtrt_pass(PRODUCER, subject);
}

static void shared_offsets(void) {
    const char *subject = "fork-offsets";
    gtrt_case_start(PRODUCER, subject);
    const char *path = "/mnt/disk/SHARED.BIN";
    int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY);
    check(fd >= 3 && write(fd, "0123456789", 10) == 10 && close(fd) == 0, subject);
    fd = open(path, O_RDONLY);
    check(fd >= 3 && sched_setforkaffinity(1) == 0, subject);
    pid_t child = fork();
    check(child >= 0, subject);
    char bytes[4];
    if (child == 0) _exit(read(fd, bytes, 4) == 4 && memcmp(bytes, "0123", 4) == 0 ? 0 : 1);
    int status = -1;
    check(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0, subject);
    check(read(fd, bytes, 4) == 4 && memcmp(bytes, "4567", 4) == 0 && close(fd) == 0, subject);
    gtrt_pass(PRODUCER, subject);
}

static void concurrent_append(void) {
    const char *subject = "concurrent-append";
    gtrt_case_start(PRODUCER, subject);
    const char *path = "/mnt/disk/APPEND.BIN";
    int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY);
    check(fd >= 3 && close(fd) == 0, subject);
    pid_t children[2];
    for (int cpu = 1; cpu <= 2; cpu++) {
        check(sched_setforkaffinity(cpu) == 0, subject);
        pid_t child = fork();
        check(child >= 0, subject);
        if (child == 0) {
            char record[64];
            memset(record, '0' + cpu, sizeof(record));
            int output = open(path, O_WRONLY | O_APPEND);
            if (output < 3) _exit(1);
            for (int i = 0; i < 16; i++) if (write(output, record, sizeof(record)) != sizeof(record)) _exit(2);
            _exit(fsync(output) == 0 && close(output) == 0 ? 0 : 3);
        }
        children[cpu - 1] = child;
    }
    for (int i = 0; i < 2; i++) {
        int status = -1;
        check(waitpid(children[i], &status, 0) == children[i] && WIFEXITED(status) && WEXITSTATUS(status) == 0, subject);
    }
    fd = open(path, O_RDONLY);
    check(fd >= 3, subject);
    unsigned counts[2] = {0};
    char record[64];
    for (int i = 0; i < 32; i++) {
        check(read(fd, record, sizeof(record)) == sizeof(record), subject);
        check(record[0] == '1' || record[0] == '2', subject);
        for (unsigned n = 1; n < sizeof(record); n++) check(record[n] == record[0], subject);
        counts[record[0] - '1']++;
    }
    check(counts[0] == 16 && counts[1] == 16 && read(fd, record, 1) == 0 && close(fd) == 0, subject);
    gtrt_pass(PRODUCER, subject);
}

int main(void) {
    gtrt_ready(PRODUCER, SUITE);
    char phase[8] = {0};
    int phase_fd = open("/.__genrt_test__/storage-phase", O_RDONLY);
    check(phase_fd >= 3 && read(phase_fd, phase, sizeof(phase) - 1) > 0 && close(phase_fd) == 0, "phase");
    int writing = strcmp(phase, "write") == 0;
    check(writing || strcmp(phase, "verify") == 0, "phase");
    boot_mount_policy(writing);
    mount_paths();
    writable_io();
    shared_offsets();
    concurrent_append();
    const char *subject = "persistence";
    gtrt_case_start(PRODUCER, subject);
    int fd = open("/mnt/disk/PERSIST.BIN", writing ? O_CREAT | O_TRUNC | O_WRONLY : O_RDONLY);
    check(fd >= 3, subject);
    check(writing ? write_pattern(fd, PERSIST_BYTES) : read_pattern(fd, PERSIST_BYTES), subject);
    check(fsync(fd) == 0 && close(fd) == 0, subject);
    gtrt_pass(PRODUCER, subject);
    gtrt_case_start(PRODUCER, "storage-poweroff");
    gtrt_terminal(PRODUCER, "storage-poweroff", "POWER_OFF");
    char *argv[] = {"poweroff", NULL};
    execve("/bin/poweroff", argv, NULL);
    gtrt_fail(PRODUCER, "storage-poweroff", "EXEC");
}
