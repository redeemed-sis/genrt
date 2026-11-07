#define main init_program_main
#include "../../user/c/init.c"
#undef main

#define CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)

static const char *input;
static size_t input_len, position;
static int opened, read_error, mounts, fail_mount, executed, closed;
static char mounted[2][64];
static unsigned warnings;
static int stderr_only;
static char warning_text[2][256];

int init_test_open(const char *path, int flags) {
    (void)flags;
    return strcmp(path, "/etc/fstab") == 0 ? opened : -2;
}
ssize_t init_test_read(int fd, void *output, size_t length) {
    (void)fd;
    if (read_error) return -GENRT_EIO;
    if (length > input_len - position) length = input_len - position;
    if (length > 7) length = 7;
    memcpy(output, input + position, length);
    position += length;
    return (ssize_t)length;
}
int init_test_close(int fd) { (void)fd; closed++; return 0; }
int init_test_execve(const char *path, char *const argv[], char *const envp[]) {
    (void)envp;
    if (strcmp(path, "/bin/shell") || strcmp(argv[0], "shell") || argv[1]) return -22;
    executed++;
    return -GENRT_ENOENT;
}
int init_test_dprintf(int fd, const char *format, ...) {
    stderr_only &= fd == 2;
    if (strncmp(format, "init: warning:", 14) == 0) {
        if (warnings < 2) {
            va_list args;
            va_start(args, format);
            vsnprintf(warning_text[warnings], sizeof(warning_text[0]), format, args);
            va_end(args);
        }
        warnings++;
    }
    return 0;
}
long genrt_syscall5(long nr, long source, long target, long type, long flags, long data) {
    if (nr != SYS_MOUNT || !source || !type || data || (flags != 0 && flags != MS_RDONLY)) return -22;
    if (mounts < 2) snprintf(mounted[mounts], sizeof(mounted[0]), "%s", (char *)target);
    unsigned attempt = (unsigned)mounts++;
    return fail_mount & (1u << attempt) ? -GENRT_EIO : 0;
}

static void reset(const char *contents) {
    input = contents;
    input_len = strlen(contents);
    position = 0;
    opened = 3;
    read_error = mounts = fail_mount = executed = closed = 0;
    warnings = 0;
    stderr_only = 1;
}

static int parse(const char *input) {
    char text[INIT_CONFIG_MAX + 1];
    struct init_mount parsed[INIT_ENTRIES_MAX];
    unsigned count, line;
    size_t length = strlen(input);
    memcpy(text, input, length + 1);
    return init_fstab(text, length, parsed, &count, &line);
}

int main(void) {
    reset("# comments\n/dev/disk0p1 /mnt/my\\040disk vfat defaults,rw 0 00\n"
          "/dev/disk0p2 /mnt/unused fat32 ro,noauto 0 0\n"
          "/dev/disk1p3 /mnt/my\\040disk/DATA fat32 ro 0 0\n");
    CHECK(init_program_main() == 1 && mounts == 2 && executed == 1 && closed == 1);
    CHECK(strcmp(mounted[0], "/mnt/my disk") == 0 && strcmp(mounted[1], "/mnt/my disk/DATA") == 0);
    reset(""); opened = -GENRT_ENOENT;
    CHECK(init_program_main() == 1 && executed == 1 && mounts == 0 && closed == 0);
    reset("");
    CHECK(init_program_main() == 1 && executed == 1 && mounts == 0 && closed == 1);
    reset("/dev/disk0p1 /mnt/disk vfat rw 0 0\ninvalid\n");
    CHECK(init_program_main() == 1 && mounts == 0 && executed == 0);
    for (int failures = 1; failures <= 3; failures++) {
        reset("/dev/disk0p1 /mnt/disk vfat rw 0 0\n/dev/disk0p2 /mnt/disk/DATA vfat rw 0 0\n");
        fail_mount = failures;
        CHECK(init_program_main() == 1 && mounts == 2 && executed == 1 && closed == 1);
        CHECK(warnings == (failures == 3 ? 2u : 1u) && stderr_only);
        CHECK(strcmp(mounted[0], "/mnt/disk") == 0 && strcmp(mounted[1], "/mnt/disk/DATA") == 0);
        unsigned warning_index = 0;
        if (failures & 1)
            CHECK(strcmp(warning_text[warning_index++],
                         "init: warning: /etc/fstab:1 mount /dev/disk0p1 on /mnt/disk failed (-5)\n") == 0);
        if (failures & 2)
            CHECK(strcmp(warning_text[warning_index],
                         "init: warning: /etc/fstab:2 mount /dev/disk0p2 on /mnt/disk/DATA failed (-5)\n") == 0);
    }
    reset(""); read_error = 1;
    CHECK(init_program_main() == 1 && closed == 1 && executed == 0);
    const char *bad[] = {
        "/dev/disk0p1 /a vfat rw 0 0\n/dev/disk00p01 /b vfat ro 0 0",
        "/dev/disk0p1 /a ext4 rw 0 0", "/dev/disk0p1 /a vfat rw 0 1",
        "/dev/disk0p1 /a vfat unknown 0 0", "/dev/disk0p1 relative vfat rw 0 0",
        "/dev/disk0p5 /a vfat rw 0 0", "/dev/disk0p1 /a\\400 vfat rw 0 0"
    };
    for (unsigned i = 0; i < sizeof(bad) / sizeof(bad[0]); i++) CHECK(parse(bad[i]) < 0);
    static char large[INIT_CONFIG_MAX + 2];
    memset(large, ' ', INIT_CONFIG_MAX + 1);
    large[INIT_CONFIG_MAX + 1] = 0;
    reset(large);
    CHECK(init_program_main() == 1 && executed == 0 && mounts == 0);
    char rows[2048] = {0};
    for (unsigned i = 0; i < 17; i++) {
        size_t used = strlen(rows);
        snprintf(rows + used, sizeof(rows) - used, "/dev/disk%up1 /mnt/d%u vfat noauto 0 0\n", i, i);
    }
    CHECK(parse(rows) == -GENRT_EOVERFLOW);
    return 0;
}
