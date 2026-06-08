#include "syscall.h"

int main(void) {
    static const char msg[] = "hello from C ELF\n";
    write(1, msg, sizeof(msg) - 1);
    return 0;
}
