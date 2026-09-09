#pragma once

#include <getopt.h>
#include <genrt/parse.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

// Return the failing source line; callers translate any nonzero value to failure.
#define SUPPORT_CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)

static int support_vformat(char *buffer, size_t size, const char *format, ...) {
    va_list args;
    va_start(args, format);
    int result = vsnprintf(buffer, size, format, args);
    va_end(args);
    return result;
}

static int support_library(void) {
    SUPPORT_CHECK(STDIN_FILENO == 0 && STDOUT_FILENO == 1 && STDERR_FILENO == 2);
    char bytes[16];
    const char binary[] = {'a', 0, (char)0xff, 'z'};
    const char high[] = {(char)0xff, 0};
    const char low[] = {1, 0};
    const char word[] = "abcd";
    const char bounded[] = {'x'};
    SUPPORT_CHECK(strlen("") == 0 && strlen(word) == 4);
    SUPPORT_CHECK(strnlen(word, 2) == 2 && strnlen(word, 8) == 4);
    SUPPORT_CHECK(strnlen(bounded, 1) == 1 && strnlen(bounded, 0) == 0);
    SUPPORT_CHECK(strcmp(word, "abcd") == 0 && strcmp("a", "aa") < 0);
    SUPPORT_CHECK(strcmp(high, low) > 0 && strncmp(high, low, 1) > 0);
    SUPPORT_CHECK(strncmp("abcd", "abce", 3) == 0);
    SUPPORT_CHECK(strncmp("a", "b", 0) == 0);
    SUPPORT_CHECK(strncmp(bounded, bounded, 1) == 0);
    SUPPORT_CHECK(strchr(word, 'b') == word + 1);
    SUPPORT_CHECK(strchr(word, 0) == word + 4 && strchr(word, 'z') == NULL);
    SUPPORT_CHECK(memset(bytes, 0x15a, sizeof(bytes)) == bytes);
    SUPPORT_CHECK((unsigned char)bytes[15] == 0x5a);
    SUPPORT_CHECK(memcpy(bytes + 1, binary, sizeof(binary)) == bytes + 1);
    SUPPORT_CHECK(bytes[0] == 'Z' && bytes[5] == 'Z');
    SUPPORT_CHECK(memcmp(bytes + 1, binary, sizeof(binary)) == 0);
    SUPPORT_CHECK(memcmp(high, low, 1) > 0 && memcmp(high, low, 0) == 0);
    SUPPORT_CHECK(memcmp("a\0x", "a\0y", 3) < 0);
    SUPPORT_CHECK(memcpy(bytes, binary, 0) == bytes && bytes[0] == 'Z');
    memcpy(bytes, "abcdef", 7);
    SUPPORT_CHECK(memmove(bytes + 1, bytes, 6) == bytes + 1);
    SUPPORT_CHECK(memcmp(bytes, "aabcdef", 7) == 0);
    SUPPORT_CHECK(memmove(bytes, bytes + 1, 6) == bytes);
    SUPPORT_CHECK(memcmp(bytes, "abcdef", 6) == 0);
    SUPPORT_CHECK(memmove(bytes, bytes, 6) == bytes);
    SUPPORT_CHECK(memmove(bytes, binary, sizeof(binary)) == bytes);
    SUPPORT_CHECK(memcmp(bytes, binary, sizeof(binary)) == 0);

    char output[256];
    SUPPORT_CHECK(snprintf(output, sizeof(output), "%s|%c|%d|%i|%u|%x|%X|%%",
                           "text", '!', -123, 456, 789u, 0xabcdu, 0xabcdu) == 31);
    SUPPORT_CHECK(strcmp(output, "text|!|-123|456|789|abcd|ABCD|%") == 0);
    SUPPORT_CHECK(snprintf(output, sizeof(output), "%06lu %05d %4s %3c",
                           123ul, -12, "a", 'b') == 21);
    SUPPORT_CHECK(strcmp(output, "000123 -0012    a   b") == 0);
    SUPPORT_CHECK(snprintf(output, sizeof(output), "%lld %llu",
                           LLONG_MIN, ULLONG_MAX) == 41);
    SUPPORT_CHECK(strcmp(output, "-9223372036854775808 18446744073709551615") == 0);
    SUPPORT_CHECK(snprintf(output, sizeof(output), "%ld %lu %zu %zd",
                           -12l, 34ul, (size_t)56, (ptrdiff_t)-78) == 13);
    SUPPORT_CHECK(strcmp(output, "-12 34 56 -78") == 0);
    SUPPORT_CHECK(snprintf(output, sizeof(output), "%p %08p", (void *)(uintptr_t)0x1234,
                           (void *)(uintptr_t)0x12) == 15);
    SUPPORT_CHECK(strcmp(output, "0x1234 0x000012") == 0);
    SUPPORT_CHECK(snprintf(output, sizeof(output), "%zx %llX",
                           (size_t)0xab, 0x12efull) == 7);
    SUPPORT_CHECK(strcmp(output, "ab 12EF") == 0);
    SUPPORT_CHECK(support_vformat(output, sizeof(output), "%d:%s", 17, "ok") == 5);
    SUPPORT_CHECK(strcmp(output, "17:ok") == 0);
    memset(bytes, 'X', sizeof(bytes));
    SUPPORT_CHECK(snprintf(bytes + 1, 4, "%s", "abcdef") == 6);
    SUPPORT_CHECK(memcmp(bytes, "Xabc\0X", 6) == 0);
    SUPPORT_CHECK(snprintf(bytes + 1, 1, "abc") == 3 && bytes[1] == 0);
    SUPPORT_CHECK(snprintf(bytes, 0, "abc") == 3 && bytes[0] == 'X');
    SUPPORT_CHECK(snprintf(NULL, 0, "%u", 123u) == 3);
    SUPPORT_CHECK(snprintf(bytes, 4, "abc") == 3 && strcmp(bytes, "abc") == 0);
    SUPPORT_CHECK(snprintf(bytes, sizeof(bytes), "%cX", 0) == 2);
    SUPPORT_CHECK(bytes[0] == 0 && bytes[1] == 'X' && bytes[2] == 0);
    SUPPORT_CHECK(snprintf(bytes, sizeof(bytes), "") == 0 && bytes[0] == 0);
    SUPPORT_CHECK(snprintf(bytes, sizeof(bytes), "%2147483647u", 1u) == INT_MAX);
    SUPPORT_CHECK(snprintf(bytes, sizeof(bytes), "%2147483648u", 1u) == -GENRT_EOVERFLOW);
    SUPPORT_CHECK(snprintf(bytes, sizeof(bytes), "x%2147483647u", 1u) == -GENRT_EOVERFLOW);
    const char *bad_formats[] = {"%", "%0", "%l", "%ll", "%z", "%ls", "%lc", "%n", "%f", "%q"};
    for (size_t index = 0; index < sizeof(bad_formats) / sizeof(bad_formats[0]); index++) {
        SUPPORT_CHECK(snprintf(bytes, sizeof(bytes), bad_formats[index]) == -GENRT_EINVAL);
        SUPPORT_CHECK(bytes[0] == 0);
    }
    SUPPORT_CHECK(dprintf(-123, "%s", "bad fd") == -9);
    SUPPORT_CHECK(printf("support output %u\n", 17u) == 18);
    SUPPORT_CHECK(dprintf(STDERR_FILENO, "support stderr\n") == 15);

    uint32_t number = 123;
    SUPPORT_CHECK(genrt_parse_u32_decimal("0", 0, &number) == 0 && number == 0);
    SUPPORT_CHECK(genrt_parse_u32_decimal("9", 0, &number) == -GENRT_ERANGE && number == 0);
    SUPPORT_CHECK(genrt_parse_u32_decimal("0007", 7, &number) == 0 && number == 7);
    SUPPORT_CHECK(genrt_parse_u32_decimal("8", 7, &number) == -GENRT_ERANGE && number == 7);
    SUPPORT_CHECK(genrt_parse_u32_decimal("4294967295", UINT32_MAX, &number) == 0);
    SUPPORT_CHECK(number == UINT32_MAX);
    SUPPORT_CHECK(genrt_parse_u32_decimal("4294967296", UINT32_MAX, &number) == -GENRT_ERANGE);
    SUPPORT_CHECK(genrt_parse_u32_decimal("2147483648", INT_MAX, &number) == -GENRT_ERANGE);
    const char *invalid[] = {"", "-1", "+1", " 1", "1 ", "1x", "0xff", "1.5"};
    for (size_t index = 0; index < sizeof(invalid) / sizeof(invalid[0]); index++) {
        SUPPORT_CHECK(genrt_parse_u32_decimal(invalid[index], UINT32_MAX, &number) == -GENRT_EINVAL);
        SUPPORT_CHECK(number == UINT32_MAX);
    }

    char *options[] = {"tool", "-ab", "-n42", "-n", "7", "--", "-x", NULL};
    optind = 0;
    opterr = 0;
    SUPPORT_CHECK(getopt(7, options, "abn:") == 'a' && optind == 1 && optarg == NULL);
    SUPPORT_CHECK(getopt(7, options, "abn:") == 'b' && optind == 2);
    SUPPORT_CHECK(getopt(7, options, "abn:") == 'n' && strcmp(optarg, "42") == 0 && optind == 3);
    SUPPORT_CHECK(getopt(7, options, "abn:") == 'n' && strcmp(optarg, "7") == 0 && optind == 5);
    SUPPORT_CHECK(getopt(7, options, "abn:") == -1 && optind == 6);
    char *operand[] = {"tool", "file", "-a", NULL};
    optind = 1;
    SUPPORT_CHECK(getopt(3, operand, "a") == -1 && optind == 1);
    char *dash[] = {"tool", "-", "-a", NULL};
    SUPPORT_CHECK(getopt(3, dash, "a") == -1 && optind == 1);
    char *unknown[] = {"tool", "-xa", NULL};
    SUPPORT_CHECK(getopt(2, unknown, "a") == '?' && optopt == 'x');
    SUPPORT_CHECK(getopt(2, unknown, "a") == 'a');
    SUPPORT_CHECK(getopt(2, unknown, "a") == -1);
    char *missing[] = {"tool", "-n", NULL};
    optind = 1;
    SUPPORT_CHECK(getopt(2, missing, ":n:") == ':' && optopt == 'n' && optarg == NULL);
    optind = 0;
    SUPPORT_CHECK(getopt(2, missing, "n:") == '?' && optopt == 'n');
    optind = 0;
    SUPPORT_CHECK(getopt(7, options, "abn:") == 'a');
    optind = 0;
    SUPPORT_CHECK(getopt(7, options, "abn:") == 'a');
    optind = 0;
    opterr = 1;
    return 0;
}

#undef SUPPORT_CHECK
