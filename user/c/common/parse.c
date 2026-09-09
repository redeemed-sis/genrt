#include <genrt/parse.h>

#include <stddef.h>

int genrt_parse_u32_decimal(const char *text, uint32_t max_value,
                            uint32_t *value) {
    if (text == NULL || value == NULL || text[0] == '\0') {
        return -GENRT_EINVAL;
    }

    uint32_t result = 0;
    for (size_t index = 0; text[index] != '\0'; index++) {
        const unsigned char character = (unsigned char)text[index];
        if (character < '0' || character > '9') {
            return -GENRT_EINVAL;
        }
        const uint32_t digit = (uint32_t)(character - '0');
        if (digit > max_value || result > max_value / 10U
            || (result == max_value / 10U && digit > max_value % 10U)) {
            return -GENRT_ERANGE;
        }
        result = result * 10U + digit;
    }
    *value = result;
    return 0;
}
