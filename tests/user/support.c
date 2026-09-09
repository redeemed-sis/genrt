#include "../qemu/user/support_case.h"
#include <unistd.h>

// --wrap=write replaces library calls while __real_write retains host output
// for test diagnostics, independent of the simulated failure state.
extern ssize_t __real_write(int fd, const void *buffer, size_t count);
static char captured[8192];
static size_t captured_size;
static size_t write_limit = sizeof(captured);
static int write_error;
static size_t fail_after = sizeof(captured);
static int last_fd;

ssize_t __wrap_write(int fd, const void *buffer, size_t count) {
    last_fd = fd;
    if (fd < 0) return -9;
    if (captured_size >= fail_after) return write_error;
    if (count > write_limit) count = write_limit;
    if (count > sizeof(captured) - captured_size) return -GENRT_EIO;
    memcpy(captured + captured_size, buffer, count);
    captured_size += count;
    return (ssize_t)count;
}

#define CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)

static int output_backend(void) {
    char large[1025];
    memset(large, 'q', sizeof(large) - 1);
    large[sizeof(large) - 1] = 0;
    captured_size = 0;
    write_limit = 3;
    CHECK(dprintf(STDERR_FILENO, "<%s>", large) == 1026);
    CHECK(last_fd == STDERR_FILENO && captured_size == 1026);
    CHECK(captured[0] == '<' && captured[1025] == '>');
    CHECK(memcmp(captured + 1, large, 1024) == 0);
    captured_size = 0;
    fail_after = 0;
    write_error = -9;
    CHECK(printf("hello") == -9 && captured_size == 0);
    CHECK(last_fd == STDOUT_FILENO);
    write_error = 0;
    CHECK(printf("hello") == -GENRT_EIO && captured_size == 0);
    fail_after = 3;
    write_error = -9;
    CHECK(printf("hello") == -9 && captured_size == 3);
    CHECK(memcmp(captured, "hel", 3) == 0);
    captured_size = 0;
    fail_after = sizeof(captured);
    char *args[] = {"tool", "-x", NULL};
    optind = 0;
    opterr = 1;
    CHECK(getopt(2, args, "a") == '?' && last_fd == STDERR_FILENO);
    CHECK(captured_size == strlen("tool: illegal option -- x\n"));
    CHECK(memcmp(captured, "tool: illegal option -- x\n", captured_size) == 0);
    captured_size = 0;
    optind = 0;
    CHECK(getopt(2, args, ":a") == '?' && captured_size == 0);
    return 0;
}

int main(void) {
    int line = support_library();
    const char *test = "library";
    if (line == 0) {
        line = output_backend();
        test = "backend";
    }
    if (line != 0) {
        char error[80];
        int count = snprintf(error, sizeof(error), "support %s failed at line %d\n", test, line);
        if (count > 0) __real_write(STDERR_FILENO, error, (size_t)count);
        return 1;
    }
    return 0;
}
