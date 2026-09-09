#pragma once

extern char *optarg;
extern int optind;
extern int opterr;
extern int optopt;

/*
 * Parse POSIX-style short options from argv. A leading ':' in optstring makes
 * a missing argument return ':'; otherwise unknown and missing options return
 * '?'. Parsing stops at a non-option or --. Callers reset optind to one after
 * a completed parse before reusing the parser with another argv.
 */
int getopt(int argc, char *const argv[], const char *optstring);
