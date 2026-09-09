#include <sched.h>
#include <genrt/parse.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define TASKSET_PATH_PREFIX "/bin/"
#define TASKSET_CPU_MAX 2147483647U
#define TASKSET_FAILURE 1
#define TASKSET_USAGE 2

// Keep pathname scratch space out of the initial stack. A valid exec argument
// vector may already occupy nearly all of that fixed 64 KiB mapping.
static char program_path[GENRT_PATH_MAX + 1];

int main(int argc, char **argv) {
    if (argc < 3 || argv[2][0] == '\0') {
        dprintf(STDERR_FILENO, "%s", "taskset: usage: taskset <cpu> <program> [args...]\n");
        return TASKSET_USAGE;
    }

    uint32_t cpu;
    if (genrt_parse_u32_decimal(argv[1], TASKSET_CPU_MAX, &cpu) != 0) {
        dprintf(STDERR_FILENO, "%s", "taskset: invalid cpu\n");
        return TASKSET_USAGE;
    }

    const char *path = argv[2];
    if (strchr(path, '/') == NULL) {
        int length = snprintf(program_path, sizeof(program_path), "%s%s",
                              TASKSET_PATH_PREFIX, path);
        if (length < 0 || (size_t)length >= sizeof(program_path)) {
            dprintf(STDERR_FILENO, "%s", "taskset: program path too long\n");
            return TASKSET_USAGE;
        }
        path = program_path;
    }

    if (sched_setforkaffinity((int)cpu) != 0) {
        dprintf(STDERR_FILENO, "%s", "taskset: unavailable cpu\n");
        return TASKSET_FAILURE;
    }

    pid_t child = fork();
    if (child < 0) {
        dprintf(STDERR_FILENO, "%s", "taskset: fork failed\n");
        return TASKSET_FAILURE;
    }
    if (child == 0) {
        execve(path, &argv[2], NULL);
        dprintf(STDERR_FILENO, "%s", "taskset: exec failed\n");
        exit(127);
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child) {
        dprintf(STDERR_FILENO, "%s", "taskset: wait failed\n");
        return TASKSET_FAILURE;
    }
    if (!WIFEXITED(status)) {
        dprintf(STDERR_FILENO, "%s", "taskset: child faulted\n");
        return TASKSET_FAILURE;
    }
    return WEXITSTATUS(status);
}
