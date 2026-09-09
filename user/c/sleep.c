#include <unistd.h>
#include <genrt/parse.h>
#include <stdio.h>

int main(int argc, char **argv) {
    uint32_t seconds = 0;
    if (argc != 2 || genrt_parse_u32_decimal(argv[1], UINT32_MAX, &seconds) != 0) {
        dprintf(STDERR_FILENO, "usage: sleep <seconds>\n");
        return 2;
    }
    return sleep(seconds) == 0 ? 0 : 1;
}
