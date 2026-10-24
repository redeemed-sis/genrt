#include "protocol.h"
#include <string.h>

int main(void) {
    const char *producer = "vfs-supervisor";
    const char *cases[] = {"mount-paths", "generated-instances", "writable-io",
        "bounded-io", "fork-exec", "fd-capacity", "cleanup", "concurrent-io", "pending-progress", "deferred-mutations"};
    gtrt_ready(producer, "vfs-contract");
    for (unsigned i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        gtrt_case_start(producer, cases[i]);
        pid_t child = fork();
        if (child < 0) gtrt_fail(producer, cases[i], "FORK");
        if (child == 0) {
            char *argv[] = {"vfs-case", (char *)cases[i], NULL};
            execve("/.__genrt_test__/bin/vfs-case", argv, NULL);
            exit(126);
        }
        int status = 0;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status)
            || WEXITSTATUS(status) != 0) gtrt_fail(producer, cases[i], "CHILD_STATUS");
        gtrt_pass(producer, cases[i]);
    }
    gtrt_done(producer, "vfs-contract");
}
