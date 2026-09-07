#include <unistd.h>

static void usage(void) {
    static const char message[] = "usage: sleep <seconds>\n";
    write(2, message, sizeof(message) - 1);
}

static int parse_seconds(const char *text, unsigned int *value) {
    if (text[0] == '\0') {
        return 0;
    }

    unsigned int result = 0;
    for (size_t index = 0; text[index] != '\0'; index++) {
        unsigned int digit = (unsigned int)(text[index] - '0');
        if (digit > 9u || result > (0xffffffffu - digit) / 10u) {
            return 0;
        }
        result = result * 10u + digit;
    }
    *value = result;
    return 1;
}

int main(int argc, char **argv) {
    unsigned int seconds = 0;
    if (argc != 2 || !parse_seconds(argv[1], &seconds)) {
        usage();
        return 2;
    }
    return sleep(seconds) == 0 ? 0 : 1;
}
