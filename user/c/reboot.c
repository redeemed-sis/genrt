#include <sys/reboot.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    if (reboot(RB_AUTOBOOT) < 0) {
        dprintf(STDERR_FILENO, "reboot: request failed\n");
    }
    return 1;
}
