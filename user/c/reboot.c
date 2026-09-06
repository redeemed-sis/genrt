#include <sys/reboot.h>

int main(void) {
    if (reboot(RB_AUTOBOOT) < 0) {
        write(2, "reboot: request failed\n", 23);
    }
    return 1;
}
