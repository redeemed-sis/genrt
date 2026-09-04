#include <sched.h>

#define TASKSET_PATH_PREFIX "/bin/"
#define TASKSET_CPU_MAX 2147483647U
#define TASKSET_FAILURE 1
#define TASKSET_USAGE 2

// Keep pathname scratch space out of the initial stack. A valid exec argument
// vector may already occupy nearly all of that fixed 64 KiB mapping.
static char program_path[GENRT_PATH_MAX + 1];

static size_t string_length(const char *text) {
    size_t length = 0;
    while (text[length] != '\0') {
        length++;
    }
    return length;
}

static void report(const char *message) {
    write(2, message, string_length(message));
}

static int parse_cpu(const char *text, int *cpu) {
    if (text[0] == '\0') {
        return -1;
    }

    unsigned int value = 0;
    for (size_t index = 0; text[index] != '\0'; index++) {
        unsigned int digit = (unsigned int)(text[index] - '0');
        if (digit > 9
            || value > (TASKSET_CPU_MAX - digit) / 10U) {
            return -1;
        }
        value = value * 10U + digit;
    }

    *cpu = (int)value;
    return 0;
}

static int contains_slash(const char *text) {
    for (size_t index = 0; text[index] != '\0'; index++) {
        if (text[index] == '/') {
            return 1;
        }
    }
    return 0;
}

static int make_program_path(char *path, size_t capacity,
                             const char *program) {
    size_t prefix_length = sizeof(TASKSET_PATH_PREFIX) - 1;
    size_t program_length = string_length(program);
    if (prefix_length + program_length + 1 > capacity) {
        return -1;
    }

    for (size_t index = 0; index < prefix_length; index++) {
        path[index] = TASKSET_PATH_PREFIX[index];
    }
    for (size_t index = 0; index < program_length; index++) {
        path[prefix_length + index] = program[index];
    }
    path[prefix_length + program_length] = '\0';
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 3 || argv[2][0] == '\0') {
        report("taskset: usage: taskset <cpu> <program> [args...]\n");
        return TASKSET_USAGE;
    }

    int cpu;
    if (parse_cpu(argv[1], &cpu) != 0) {
        report("taskset: invalid cpu\n");
        return TASKSET_USAGE;
    }

    const char *path = argv[2];
    if (!contains_slash(path)) {
        if (make_program_path(program_path, sizeof(program_path), path) != 0) {
            report("taskset: program path too long\n");
            return TASKSET_USAGE;
        }
        path = program_path;
    }

    if (sched_setforkaffinity(cpu) != 0) {
        report("taskset: unavailable cpu\n");
        return TASKSET_FAILURE;
    }

    pid_t child = fork();
    if (child < 0) {
        report("taskset: fork failed\n");
        return TASKSET_FAILURE;
    }
    if (child == 0) {
        execve(path, &argv[2], NULL);
        report("taskset: exec failed\n");
        exit(127);
    }

    int status = 0;
    if (waitpid(child, &status, 0) != child) {
        report("taskset: wait failed\n");
        return TASKSET_FAILURE;
    }
    if (!WIFEXITED(status)) {
        report("taskset: child faulted\n");
        return TASKSET_FAILURE;
    }
    return WEXITSTATUS(status);
}
