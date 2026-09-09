#include <getopt.h>

#include <stdio.h>
#include <string.h>
#include <unistd.h>

char *optarg;
int optind = 1;
int opterr = 1;
int optopt;

static const char *next_option;

int getopt(int argc, char *const argv[], const char *optstring) {
    optarg = NULL;
    if (optind <= 0) {
        optind = 1;
        next_option = NULL;
    }
    if (next_option == NULL || *next_option == '\0') {
        if (optind >= argc || argv[optind][0] != '-' || argv[optind][1] == '\0') {
            next_option = NULL;
            return -1;
        }
        if (strcmp(argv[optind], "--") == 0) {
            optind++;
            next_option = NULL;
            return -1;
        }
        next_option = argv[optind] + 1;
    }

    const char option = *next_option++;
    const char *specification = strchr(optstring, option);
    if (option == ':' || specification == NULL) {
        optopt = (unsigned char)option;
        if (*next_option == '\0') {
            optind++;
            next_option = NULL;
        }
        if (opterr != 0 && optstring[0] != ':') {
            dprintf(STDERR_FILENO, "%s: illegal option -- %c\n", argv[0], option);
        }
        return '?';
    }
    if (specification[1] != ':') {
        if (*next_option == '\0') {
            optind++;
            next_option = NULL;
        }
        return option;
    }
    if (*next_option != '\0') {
        optarg = (char *)next_option;
        optind++;
        next_option = NULL;
        return option;
    }
    if (optind + 1 < argc) {
        optarg = argv[optind + 1];
        optind += 2;
        next_option = NULL;
        return option;
    }

    optopt = (unsigned char)option;
    optind++;
    next_option = NULL;
    if (opterr != 0 && optstring[0] != ':') {
        dprintf(STDERR_FILENO, "%s: option requires an argument -- %c\n", argv[0], option);
    }
    return optstring[0] == ':' ? ':' : '?';
}
