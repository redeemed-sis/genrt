#include <sys/reboot.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    if (reboot(RB_POWER_OFF) < 0) {
        dprintf(STDERR_FILENO, "poweroff: request failed\n");
    }
    return 1;
}
