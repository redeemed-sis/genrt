#include "artifact_marker.h"
#include "syscall.h"
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <time.h>

#define A "/.__genrt_test__/mount-a"
#define B "/.__genrt_test__/mount-b"
#define SELF "/.__genrt_test__/bin/vfs-case"
#define EIO 5
#define EBADF 9
#define ENOENT 2
#define EFAULT 14
#define ENOTDIR 20
#define EISDIR 21
#define EINVAL 22
#define EMFILE 24
#define EROFS 30
#define ERANGE 34
#define ENAMETOOLONG 36
#define BAD ((void *)0x0000006000000000UL)

static int failure(int line) {
    char text[64];
    int n = snprintf(text, sizeof(text), "vfs assertion line %d\n", line);
    if (n > 0) write(STDERR_FILENO, text, (size_t)n);
    return 1;
}
#define CHECK(x) do { if (!(x)) return failure(__LINE__); } while (0)

struct stats {
    unsigned long opens, closes, live, pending, completed;
    unsigned long dc, dop, dt, dw, rc, ro, rt, rw;
};

static int stats(struct stats *s) {
    char text[192];
    int fd = open(A "/stats", O_RDONLY);
    CHECK(fd >= 3);
    long n = read(fd, text, sizeof(text) - 1);
    CHECK(n > 0);
    text[n] = 0;
    CHECK(close(fd) == 0);
    const char *keys[] = {"opens=", " closes=", " live=", " pending=", " completed=",
        " dc=", " do=", " dt=", " dw=", " rc=", " ro=", " rt=", " rw="};
    unsigned long *values[] = {&s->opens, &s->closes, &s->live, &s->pending, &s->completed,
        &s->dc, &s->dop, &s->dt, &s->dw, &s->rc, &s->ro, &s->rt, &s->rw};
    const char *p = text;
    for (unsigned i = 0; i < sizeof(keys) / sizeof(keys[0]); i++) {
        size_t len = strlen(keys[i]);
        CHECK(strncmp(p, keys[i], len) == 0);
        p += len;
        CHECK(*p >= '0' && *p <= '9');
        *values[i] = 0;
        while (*p >= '0' && *p <= '9') *values[i] = *values[i] * 10 + (unsigned)(*p++ - '0');
    }
    CHECK(strcmp(p, "\n") == 0);
    return 0;
}

static int wait_ok(pid_t child) {
    int status = 0;
    CHECK(child > 0 && waitpid(child, &status, 0) == child);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    return 0;
}

static int contents(const char *path, const char *expected) {
    char data[128];
    int fd = open(path, O_RDONLY);
    CHECK(fd >= 3);
    size_t length = strlen(expected), done = 0;
    while (done < length) {
        long n = read(fd, data + done, 3);
        CHECK(n > 0 && (size_t)n <= length - done);
        done += (size_t)n;
    }
    CHECK(memcmp(data, expected, length) == 0 && read(fd, data, 1) == 0);
    CHECK(close(fd) == 0);
    return 0;
}

static int put(const char *path, const char *data) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC);
    CHECK(fd >= 3 && write(fd, data, strlen(data)) == (long)strlen(data));
    CHECK(close(fd) == 0);
    return 0;
}

static int entry(int fd, const char *name, long cookie, int type) {
    unsigned char bytes[256] __attribute__((aligned(8)));
    memset(bytes, 0xa5, sizeof(bytes));
    long n = getdents64(fd, bytes, sizeof(bytes) - 8);
    CHECK(n >= 24 && n <= (long)sizeof(bytes) - 8 && n % 8 == 0);
    struct genrt_dirent64 *e = (struct genrt_dirent64 *)bytes;
    CHECK(e->d_reclen == n && e->d_off == cookie && e->d_type == type);
    CHECK(e->d_ino != 0 && strcmp(e->d_name, name) == 0);
    for (size_t i = (size_t)n; i < sizeof(bytes); i++) CHECK(bytes[i] == 0xa5);
    return 0;
}

static int paths(void) {
    char cwd[128], tiny[2] = {'x', 'y'};
    CHECK(chdir(A "/sub") == 0);
    CHECK(getcwd(cwd, sizeof(cwd)) == cwd && strcmp(cwd, A "/sub") == 0);
    CHECK(contents("seed", "seed:a\n") == 0);
    CHECK(open(A "/covered", O_RDONLY) == -ENOENT);
    CHECK(chdir("..//../mount-b/./sub/..") == 0);
    CHECK(getcwd(cwd, sizeof(cwd)) && strcmp(cwd, B) == 0);
    CHECK(contents("dynamic", "dynamic:b\n") == 0);
    CHECK(chdir("../mount-a") == 0);
    CHECK(open("missing/../dynamic", O_RDONLY) == -ENOENT);
    CHECK(open("dynamic/..", O_RDONLY) == -ENOTDIR);
    CHECK(open("dynamic/", O_RDONLY) == -ENOTDIR);
    CHECK(open("dynamic/.", O_RDONLY) == -ENOTDIR);
    CHECK(chdir("dynamic") == -ENOTDIR && chdir("missing") == -ENOENT);
    CHECK(getcwd(cwd, sizeof(cwd)) && strcmp(cwd, A) == 0);
    CHECK(genrt_syscall2(SYS_GETCWD, (long)tiny, sizeof(tiny)) == -ERANGE);
    CHECK(tiny[0] == 'x' && tiny[1] == 'y');
    CHECK(genrt_syscall2(SYS_GETCWD, (long)BAD, sizeof(cwd)) == -EFAULT);
    CHECK(open("", O_RDONLY) == -ENOENT && open(BAD, O_RDONLY) == -EFAULT);
    CHECK(open("must-not-create", O_CREAT | O_DIRECTORY) == -EINVAL);
    CHECK(open("must-not-create", O_RDONLY) == -ENOENT);
    static char longname[4097];
    memset(longname, 'q', 4096); longname[4096] = 0;
    CHECK(open(longname, O_CREAT | O_WRONLY) == -ENAMETOOLONG);
    CHECK(open(longname, O_RDONLY) == -ENAMETOOLONG);
    CHECK(chdir("/../../") == 0 && getcwd(cwd, sizeof(cwd)) && strcmp(cwd, "/") == 0);
    int fd = open(A "/sub", O_DIRECTORY);
    CHECK(fd >= 3 && getdents64(fd, cwd, 1) == -EINVAL);
    CHECK(read(fd, cwd, sizeof(cwd)) == -EISDIR);
    CHECK(getdents64(fd, BAD, 128) == -EFAULT);
    CHECK(entry(fd, "seed", 1, DT_REG) == 0);
    CHECK(getdents64(fd, cwd, sizeof(cwd)) == 0 && close(fd) == 0);
    fd = open(A "/dynamic", O_RDONLY);
    CHECK(fd >= 3 && getdents64(fd, cwd, sizeof(cwd)) == -ENOTDIR && close(fd) == 0);
    fd = open(A, O_DIRECTORY);
    CHECK(fd >= 3);
    const char *names[] = {"dynamic", "errors", "open-error", "pending", "pending-control", "stats", "sub"};
    for (unsigned i = 0; i < 7; i++) CHECK(entry(fd, names[i], i + 1, i == 6 ? DT_DIR : DT_REG) == 0);
    CHECK(getdents64(fd, cwd, sizeof(cwd)) == 0 && close(fd) == 0);
    return 0;
}

static int generated(void) {
    CHECK(contents(A "/dynamic", "dynamic:a\n") == 0);
    CHECK(contents(B "/dynamic", "dynamic:b\n") == 0);
    CHECK(put(A "/instance", "left") == 0 && put(B "/instance", "right") == 0);
    CHECK(contents(A "/instance", "left") == 0 && contents(B "/instance", "right") == 0);
    return 0;
}

static int writable(void) {
    char data[32];
    int fd = open(A "/write", O_CREAT | O_RDWR);
    CHECK(fd >= 3 && write(fd, "ab", 2) == 2 && write(fd, "cd", 2) == 2);
    CHECK(read(fd, data, sizeof(data)) == 0 && close(fd) == 0);
    CHECK(contents(A "/write", "abcd") == 0);
    fd = open(A "/write", O_WRONLY | O_APPEND);
    int other = open(A "/write", O_WRONLY | O_APPEND);
    CHECK(fd >= 3 && other >= 3);
    CHECK(write(fd, "ef", 2) == 2 && write(other, "gh", 2) == 2 && write(fd, "ij", 2) == 2);
    CHECK(read(fd, data, 1) == -EBADF && close(fd) == 0 && close(other) == 0);
    CHECK(contents(A "/write", "abcdefghij") == 0);
    fd = open(A "/write", O_RDWR | O_TRUNC);
    CHECK(fd >= 3 && read(fd, data, 1) == 0 && close(fd) == 0);
    CHECK(open(A "/write", O_RDONLY | O_TRUNC) == -EINVAL);
    CHECK(open(A "/write", 3) == -EINVAL && open(A "/write", 0x40000000) == -EINVAL);
    CHECK(open(A "/sub", O_WRONLY) == -EISDIR);
    CHECK(open(A "/dynamic", O_DIRECTORY) == -ENOTDIR);
    const int flags[] = {O_WRONLY, O_RDWR, O_WRONLY | O_APPEND, O_WRONLY | O_TRUNC};
    for (unsigned i = 0; i < sizeof(flags) / sizeof(flags[0]); i++)
        CHECK(open("/.__genrt_test__/fixtures/known-content", flags[i]) == -EROFS);
    CHECK(open("/.__genrt_test__/fixtures/new", O_CREAT | O_WRONLY) == -EROFS);
    fd = open(A "/errors", O_RDWR);
    CHECK(fd >= 3 && read(fd, data, 1) == -EIO && write(fd, "x", 1) == -EIO && close(fd) == 0);
    CHECK(open(A "/open-error", O_RDONLY) == -EIO);
    return 0;
}

static int large_io(void) {
    static unsigned char source[4099], dest[4101];
    for (size_t i = 0; i < sizeof(source); i++) source[i] = (unsigned char)(i * 37u + 11u);
    int fd = open(A "/large", O_CREAT | O_RDWR | O_TRUNC);
    CHECK(fd >= 3 && write(fd, BAD, 8) == -EFAULT);
    size_t total = 0;
    while (total < sizeof(source)) {
        long n = write(fd, source + total, sizeof(source) - total);
        CHECK(n > 0 && n <= 1024 && (size_t)n <= sizeof(source) - total);
        total += (size_t)n;
    }
    CHECK(close(fd) == 0);
    fd = open(A "/large", O_RDONLY);
    CHECK(fd >= 3 && read(fd, BAD, 8) == -EFAULT);
    static const char readonly[8] = {0};
    CHECK(read(fd, (void *)readonly, 8) == -EFAULT);
    /* The controlled user stack ends eight bytes after this address. */
    volatile unsigned char *edge = (volatile unsigned char *)0x0000007ffffffff8UL;
    unsigned char saved[8];
    for (unsigned i = 0; i < 8; i++) { saved[i] = edge[i]; edge[i] = 0xa5; }
    long partial = read(fd, (void *)edge, 16);
    int unchanged = 1;
    for (unsigned i = 0; i < 8; i++) { if (edge[i] != 0xa5) unchanged = 0; edge[i] = saved[i]; }
    CHECK(partial == -EFAULT && unchanged);
    CHECK(write(fd, "x", 1) == -EBADF);
    memset(dest, 0xa5, sizeof(dest)); total = 0;
    while (total < sizeof(source)) {
        long n = read(fd, dest + 1 + total, sizeof(source) - total);
        CHECK(n > 0 && n <= 1024 && (size_t)n <= sizeof(source) - total);
        total += (size_t)n;
    }
    CHECK(dest[0] == 0xa5 && dest[4100] == 0xa5);
    CHECK(memcmp(dest + 1, source, sizeof(source)) == 0 && read(fd, dest, 1) == 0);
    CHECK(close(fd) == 0);
    /* A rejected component must not leave an unenumerable created file. */
    char path[sizeof(A) + 258];
    memcpy(path, A "/", sizeof(A));
    memset(path + sizeof(A), 'q', 256);
    path[sizeof(A) + 256] = 0;
    CHECK(open(path, O_CREAT | O_WRONLY) == -ENAMETOOLONG);
    path[sizeof(A) + 255] = 0;
    CHECK(put(path, "boundary") == 0 && contents(path, "boundary") == 0);
    fd = open(A, O_DIRECTORY); CHECK(fd >= 3);
    unsigned found = 0;
    unsigned char directory[1024] __attribute__((aligned(8)));
    for (;;) {
        long n = getdents64(fd, directory, sizeof(directory)); CHECK(n >= 0);
        if (n == 0) break;
        struct genrt_dirent64 *e = (struct genrt_dirent64 *)directory;
        CHECK(e->d_ino != 0 && e->d_reclen == n && strlen(e->d_name) <= 255);
        if (!strcmp(e->d_name, path + sizeof(A))) found++;
    }
    CHECK(found == 1 && close(fd) == 0);
    return 0;
}

static int inheritance(void) {
    char bytes[4], cwd[128];
    CHECK(put(A "/shared", "abcdef") == 0);
    int fd = open(A "/shared", O_RDONLY), independent = open(A "/shared", O_RDONLY);
    CHECK(fd >= 3 && independent >= 3 && read(fd, bytes, 2) == 2 && memcmp(bytes, "ab", 2) == 0);
    pid_t child = fork(); CHECK(child >= 0);
    if (child == 0) {
        CHECK(read(fd, bytes, 2) == 2 && memcmp(bytes, "cd", 2) == 0);
        CHECK(close(fd) == 0 && chdir(B "/sub") == 0); return 0;
    }
    CHECK(wait_ok(child) == 0 && read(fd, bytes, 2) == 2 && memcmp(bytes, "ef", 2) == 0);
    CHECK(getcwd(cwd, sizeof(cwd)) && strcmp(cwd, "/") == 0);
    CHECK(read(independent, bytes, 2) == 2 && memcmp(bytes, "ab", 2) == 0);
    CHECK(close(fd) == 0 && close(independent) == 0);
    fd = open("/.__genrt_test__/fixtures/directory", O_DIRECTORY);
    CHECK(fd >= 3 && entry(fd, "a", 1, DT_REG) == 0);
    child = fork(); CHECK(child >= 0);
    if (child == 0) { CHECK(entry(fd, "b", 2, DT_REG) == 0); return 0; }
    CHECK(wait_ok(child) == 0 && getdents64(fd, bytes, sizeof(bytes)) == 0 && close(fd) == 0);
    CHECK(chdir(A "/sub") == 0);
    fd = open("seed", O_RDONLY); CHECK(fd == 3 && read(fd, bytes, 2) == 2);
    int dir = open(".", O_DIRECTORY); CHECK(dir == 4);
    int writable = open(A "/exec-write", O_CREAT | O_RDWR | O_TRUNC);
    CHECK(writable == 5 && write(writable, "ab", 2) == 2);
    CHECK(execve("missing", NULL, NULL) == -ENOENT);
    CHECK(entry(dir, "seed", 1, DT_REG) == 0);
    char *args[] = {"vfs-case", "exec-probe", NULL};
    execve(SELF, args, NULL);
    return failure(__LINE__);
}

static int exec_probe(void) {
    char cwd[128], bytes[32];
    CHECK(getcwd(cwd, sizeof(cwd)) && strcmp(cwd, A "/sub") == 0);
    CHECK(read(3, bytes, sizeof(bytes)) == 5 && memcmp(bytes, "ed:a\n", 5) == 0);
    CHECK(getdents64(4, bytes, sizeof(bytes)) == 0);
    CHECK(write(5, "cd", 2) == 2 && close(5) == 0);
    CHECK(contents(A "/exec-write", "abcd") == 0);
    CHECK(close(3) == 0 && close(4) == 0 && contents("seed", "seed:a\n") == 0);
    return 0;
}

static int capacity(void) {
    int fds[29]; char byte;
    CHECK(put(A "/write", "preserved") == 0);
    for (int i = 0; i < 29; i++) { fds[i] = open(A "/dynamic", O_RDONLY); CHECK(fds[i] == i + 3); }
    CHECK(open(A "/capacity-no-create", O_CREAT | O_WRONLY) == -EMFILE);
    CHECK(open(A "/write", O_WRONLY | O_TRUNC) == -EMFILE);
    CHECK(close(fds[7]) == 0 && open(A "/dynamic", O_RDONLY) == fds[7]);
    for (int i = 0; i < 29; i++) CHECK(close(fds[i]) == 0);
    CHECK(open(A "/capacity-no-create", O_RDONLY) == -ENOENT);
    CHECK(contents(A "/write", "preserved") == 0);
    const int invalid[] = {-1, 3, 32, 0x7fffffff};
    for (unsigned i = 0; i < sizeof(invalid) / sizeof(invalid[0]); i++) {
        CHECK(read(invalid[i], &byte, 0) == -EBADF && write(invalid[i], &byte, 0) == -EBADF);
        CHECK(read(invalid[i], &byte, 1) == -EBADF && write(invalid[i], &byte, 1) == -EBADF);
        CHECK(close(invalid[i]) == -EBADF);
    }
    int fd = open(A "/write", O_RDWR);
    CHECK(fd == 3 && read(fd, BAD, 0) == 0 && write(fd, BAD, 0) == 0 && close(fd) == 0);
    for (int i = 0; i < 3; i++) CHECK(close(i) == -EBADF);
    return 0;
}

static int cleanup(void) {
    struct stats before, after;
    CHECK(stats(&before) == 0);
    CHECK(before.opens == before.closes + before.live);
    for (int i = 0; i < 4; i++) {
        CHECK(open(A "/open-error", O_RDONLY) == -EIO);
        pid_t child = fork(); CHECK(child >= 0);
        if (child == 0) {
            CHECK(open(A "/dynamic", O_RDONLY) >= 3 && chdir(A "/sub") == 0);
            if (i == 1) { volatile unsigned char byte = *(volatile unsigned char *)BAD; (void)byte; return 42; }
            if (i == 3) { genrt_syscall0(0x7fffffff); return 42; }
            return 0;
        }
        int status = 0;
        CHECK(waitpid(child, &status, 0) == child);
        CHECK((i & 1) ? status == 0x7f : (WIFEXITED(status) && WEXITSTATUS(status) == 0));
    }
    CHECK(stats(&after) == 0 && after.live == before.live);
    CHECK(after.opens == after.closes + after.live);
    CHECK(after.opens - before.opens == 9 && after.closes - before.closes == 9);
    int fd = open(A "/dynamic", O_RDONLY); CHECK(fd >= 3);
    CHECK(stats(&before) == 0);
    pid_t child = fork(); CHECK(child >= 0);
    if (child == 0) { CHECK(close(fd) == 0); return 0; }
    CHECK(wait_ok(child) == 0 && stats(&after) == 0 && after.live == before.live);
    CHECK(close(fd) == 0 && stats(&after) == 0 && after.live + 1 == before.live);
    CHECK(stats(&before) == 0);
    fd = open(A "/dynamic", O_RDONLY); CHECK(fd >= 3);
    pid_t children[8]; unsigned count = 0; int exhausted = 0;
    for (unsigned i = 0; i < 8; i++) {
        child = fork();
        if (child < 0) { exhausted = 1; break; }
        if (child == 0) return 0;
        children[count++] = child;
    }
    CHECK(exhausted && count > 0);
    for (unsigned i = 0; i < count; i++) CHECK(wait_ok(children[i]) == 0);
    char byte;
    CHECK(read(fd, &byte, 1) == 1 && byte == 'd' && close(fd) == 0);
    CHECK(stats(&after) == 0 && after.live == before.live);
    return 0;
}

static int worker_gate(unsigned worker) {
    CHECK(put(worker ? A "/ready-b" : A "/ready-a", "ready") == 0);
    int fd = open(A "/start", O_RDONLY); CHECK(fd >= 3);
    for (unsigned i = 0; i < 1000; i++) {
        char byte;
        long n = read(fd, &byte, 1);
        if (n == 1) { CHECK(byte == 'g' && close(fd) == 0); return 0; }
        CHECK(n == 0 && usleep(100) == 0);
    }
    return failure(__LINE__);
}

static int release_workers(void) {
    for (unsigned i = 0; i < 1000; i++) {
        char bytes[8];
        int a = open(A "/ready-a", O_RDONLY), b = open(A "/ready-b", O_RDONLY);
        CHECK(a >= 3 && b >= 3);
        long na = read(a, bytes, sizeof(bytes)), nb = read(b, bytes, sizeof(bytes));
        CHECK(close(a) == 0 && close(b) == 0);
        if (na == 5 && nb == 5) return put(A "/start", "go");
        CHECK(na >= 0 && nb >= 0 && usleep(100) == 0);
    }
    return failure(__LINE__);
}

static int reset_gate(void) {
    CHECK(put(A "/ready-a", "") == 0 && put(A "/ready-b", "") == 0);
    return put(A "/start", "");
}

static int concurrent(void) {
    unsigned char records[64];
    for (unsigned i = 0; i < sizeof(records); i++) records[i] = (unsigned char)i;
    int fd = open(A "/records", O_CREAT | O_WRONLY | O_TRUNC);
    CHECK(fd >= 3 && write(fd, records, sizeof(records)) == sizeof(records) && close(fd) == 0);
    fd = open(A "/records", O_RDONLY); CHECK(fd >= 3);
    CHECK(reset_gate() == 0);
    pid_t children[2];
    for (unsigned worker = 0; worker < 2; worker++) {
        CHECK(sched_setforkaffinity((int)worker + 1) == 0);
        children[worker] = fork(); CHECK(children[worker] >= 0);
        if (children[worker] == 0) {
            CHECK(worker_gate(worker) == 0);
            unsigned char seen[64]; size_t count = 0;
            for (;;) { long n = read(fd, seen + count, 1); CHECK(n == 0 || n == 1); if (!n) break; count++; CHECK(count <= 64); }
            const char *out = worker ? A "/seen-b" : A "/seen-a";
            int result = open(out, O_CREAT | O_WRONLY | O_TRUNC); CHECK(result >= 3);
            CHECK(write(result, seen, count) == (long)count && close(result) == 0);
            return 0;
        }
    }
    CHECK(release_workers() == 0);
    CHECK(wait_ok(children[0]) == 0 && wait_ok(children[1]) == 0 && close(fd) == 0);
    unsigned seen[64] = {0}; unsigned total = 0;
    for (unsigned worker = 0; worker < 2; worker++) {
        fd = open(worker ? A "/seen-b" : A "/seen-a", O_RDONLY); CHECK(fd >= 3);
        long n = read(fd, records, sizeof(records)); CHECK(n >= 0 && n <= 64);
        for (long j = 0; j < n; j++) { CHECK(records[j] < 64); seen[records[j]]++; total++; }
        CHECK(close(fd) == 0);
    }
    CHECK(total == 64); for (unsigned i = 0; i < 64; i++) CHECK(seen[i] == 1);
    CHECK(put(A "/append", "") == 0);
    CHECK(reset_gate() == 0);
    for (unsigned worker = 0; worker < 2; worker++) {
        CHECK(sched_setforkaffinity((int)worker + 1) == 0);
        children[worker] = fork(); CHECK(children[worker] >= 0);
        if (children[worker] == 0) {
            CHECK(worker_gate(worker) == 0);
            int out = open(A "/append", O_WRONLY | O_APPEND); CHECK(out >= 3);
            for (unsigned i = 0; i < 32; i++) { unsigned char r[4] = {(unsigned char)worker, (unsigned char)i, 0x55, 0xaa}; CHECK(write(out, r, 4) == 4); }
            CHECK(close(out) == 0); return 0;
        }
    }
    CHECK(release_workers() == 0 && wait_ok(children[0]) == 0 && wait_ok(children[1]) == 0);
    unsigned char all[256]; unsigned counts[2][32] = {{0}};
    fd = open(A "/append", O_RDONLY); CHECK(fd >= 3 && read(fd, all, sizeof(all)) == sizeof(all));
    CHECK(read(fd, records, 1) == 0 && close(fd) == 0);
    for (unsigned i = 0; i < sizeof(all); i += 4) {
        CHECK(all[i] < 2 && all[i+1] < 32 && all[i+2] == 0x55 && all[i+3] == 0xaa);
        counts[all[i]][all[i+1]]++;
    }
    for (unsigned w = 0; w < 2; w++) for (unsigned i = 0; i < 32; i++) CHECK(counts[w][i] == 1);
    return 0;
}

static int mutation_progress(unsigned long expected) {
    char text[32];
    int fd = open(A "/mutation-progress", O_RDONLY); CHECK(fd >= 3);
    long n = read(fd, text, sizeof(text) - 1); CHECK(n > 0 && close(fd) == 0);
    text[n] = 0;
    unsigned long observed = 0;
    for (long i = 0; i < n; i++) { CHECK(text[i] >= '0' && text[i] <= '9'); observed = observed * 10 + (unsigned)(text[i] - '0'); }
    CHECK(observed >= expected);
    return 0;
}

static int deferred_mutations(void) {
    struct stats before, now, after;
    CHECK(put(A "/mutation-progress", "0") == 0 && stats(&before) == 0);
    CHECK(sched_setforkaffinity(1) == 0);
    pid_t child = fork(); CHECK(child >= 0);
    if (child == 0) {
        int fd = open(A "/deferred-record", O_CREAT | O_RDWR | O_TRUNC);
        CHECK(fd >= 3 && mutation_progress(3) == 0);
        CHECK(write(fd, "old", 3) == 3 && mutation_progress(4) == 0 && close(fd) == 0);
        fd = open(A "/deferred-record", O_WRONLY | O_TRUNC);
        CHECK(fd >= 3 && mutation_progress(6) == 0);
        CHECK(write(fd, "A", 1) == 1 && mutation_progress(7) == 0 && close(fd) == 0);
        fd = open(A "/deferred-record", O_WRONLY | O_APPEND);
        CHECK(fd >= 3 && mutation_progress(8) == 0);
        CHECK(write(fd, "BC", 2) == 2 && mutation_progress(9) == 0);
        CHECK(write(fd, "DE", 2) == 2 && mutation_progress(10) == 0 && close(fd) == 0);
        return 0;
    }
    unsigned long published = 0;
    unsigned observed_stages = 0;
    for (unsigned i = 0; i < 3000; i++) {
        CHECK(stats(&now) == 0);
        unsigned long submitted = now.dc + now.dop + now.dt + now.dw
            - before.dc - before.dop - before.dt - before.dw;
        unsigned long resumed = now.rc + now.ro + now.rt + now.rw
            - before.rc - before.ro - before.rt - before.rw;
        CHECK(submitted <= 10 && resumed <= submitted);
        if (submitted > published && submitted > resumed) {
            char text[32];
            int n = snprintf(text, sizeof(text), "%lu", submitted);
            CHECK(n > 0 && (size_t)n < sizeof(text) && put(A "/mutation-progress", text) == 0);
            published = submitted;
            observed_stages++;
        }
        if (resumed == 10) break;
        CHECK(usleep(100) == 0);
    }
    CHECK(published == 10 && observed_stages == 10 && wait_ok(child) == 0);
    CHECK(stats(&after) == 0 && after.live == before.live);
    CHECK(after.dc == before.dc + 1 && after.rc == before.rc + 1);
    CHECK(after.dop == before.dop + 3 && after.ro == before.ro + 3);
    CHECK(after.dt == before.dt + 2 && after.rt == before.rt + 2);
    CHECK(after.dw == before.dw + 4 && after.rw == before.rw + 4);
    CHECK(contents(A "/deferred-record", "ABCDE") == 0);
    return 0;
}

static int pending(void) {
    struct stats before, observed, after;
    int control = open(A "/pending-control", O_WRONLY); CHECK(control >= 3);
    CHECK(write(control, "0", 1) == 1 && close(control) == 0);
    CHECK(put(A "/progress", "") == 0);
    CHECK(put(A "/pending-peer", "") == 0);
    int fd = open(A "/pending", O_RDONLY); CHECK(fd >= 3);
    CHECK(stats(&before) == 0 && sched_setforkaffinity(1) == 0);
    pid_t first = fork(); CHECK(first >= 0);
    if (first == 0) {
        char bytes[8];
        CHECK(read(fd, bytes, 5) == 5 && memcmp(bytes, "pendi", 5) == 0);
        CHECK(contents(A "/progress", "go") == 0); return 0;
    }
    int inflight = 0;
    for (unsigned i = 0; i < 1000; i++) {
        CHECK(stats(&observed) == 0);
        if (observed.pending == before.pending + 1 && observed.completed == before.completed) { inflight = 1; break; }
        CHECK(usleep(100) == 0);
    }
    CHECK(inflight);
    CHECK(sched_setforkaffinity(2) == 0);
    pid_t second = fork(); CHECK(second >= 0);
    if (second == 0) {
        CHECK(put(A "/pending-peer", "ready") == 0);
        char bytes[8]; CHECK(read(fd, bytes, 5) == 5 && memcmp(bytes, "ng:a\n", 5) == 0); return 0;
    }
    int peer_ready = 0;
    for (unsigned i = 0; i < 1000; i++) {
        int control = open(A "/pending-peer", O_RDONLY); CHECK(control >= 3);
        char bytes[8]; long n = read(control, bytes, sizeof(bytes)); CHECK(close(control) == 0);
        if (n == 5 && memcmp(bytes, "ready", 5) == 0) { peer_ready = 1; break; }
        CHECK(n == 0 && usleep(100) == 0);
    }
    /* Pending ownership must survive beyond the fixture's former 10 ms delay. */
    CHECK(peer_ready && usleep(20000) == 0 && stats(&observed) == 0);
    CHECK(observed.pending == before.pending + 1 && observed.completed == before.completed);
    CHECK(put(A "/progress", "go") == 0);
    control = open(A "/pending-control", O_WRONLY); CHECK(control >= 3);
    CHECK(write(control, "1", 1) == 1 && close(control) == 0);
    CHECK(wait_ok(first) == 0 && wait_ok(second) == 0);
    CHECK(stats(&after) == 0 && after.pending == before.pending + 1 && after.completed == before.completed + 2);
    CHECK(close(fd) == 0);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    if (!strcmp(argv[1], "mount-paths")) return paths();
    if (!strcmp(argv[1], "generated-instances")) return generated();
    if (!strcmp(argv[1], "writable-io")) return writable();
    if (!strcmp(argv[1], "bounded-io")) return large_io();
    if (!strcmp(argv[1], "fork-exec")) return inheritance();
    if (!strcmp(argv[1], "exec-probe")) return exec_probe();
    if (!strcmp(argv[1], "fd-capacity")) return capacity();
    if (!strcmp(argv[1], "cleanup")) return cleanup();
    if (!strcmp(argv[1], "concurrent-io")) return concurrent();
    if (!strcmp(argv[1], "pending-progress")) return pending();
    if (!strcmp(argv[1], "deferred-mutations")) return deferred_mutations();
    return 2;
}
