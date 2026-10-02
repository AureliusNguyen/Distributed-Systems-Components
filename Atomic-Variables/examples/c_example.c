/* Increments a shared counter that any other process (Python, Rust, C, the
 * daemon) can see.
 *
 *   gcc -I crates/atomvar-ffi/include examples/c_example.c \
 *       -L <target>/release -Wl,-rpath,<target>/release -latomvar -o c_example
 *   ./c_example demo counter 5
 */
#include "atomvar.h"

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: %s <arena> <var> <increments>\n", argv[0]);
        return 2;
    }
    atomvar_arena *arena = NULL;
    atomvar_i64 *counter = NULL;
    if (atomvar_arena_open(argv[1], 0, &arena) != ATOMVAR_OK ||
        atomvar_arena_i64(arena, argv[2], 0, &counter) != ATOMVAR_OK) {
        fprintf(stderr, "atomvar: %s\n", atomvar_last_error());
        return 1;
    }
    long n = strtol(argv[3], NULL, 10);
    int64_t now = 0;
    for (long i = 0; i < n; i++) {
        atomvar_i64_add_and_get(counter, 1, ATOMVAR_SEQ_CST, &now);
    }
    printf("%s/%s = %" PRId64 "\n", argv[1], argv[2], now);
    atomvar_i64_free(counter);
    atomvar_arena_close(arena);
    return 0;
}
