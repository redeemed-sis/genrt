#include <sys/reboot.h>

int main(void) {
    if (reboot(RB_POWER_OFF) < 0) {
        write(2, "poweroff: request failed\n", 25);
    }
    return 1;
}
