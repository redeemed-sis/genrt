#include "artifact_marker.h"
#include "syscall.h"

#define DIRENT64_HEADER_SIZE 19

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
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFEXITED(status)
                   && WEXITSTATUS(status) == 0
               ? 0
               : 2;
}

int main(int argc, char **argv) {
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
    return 65;
}
