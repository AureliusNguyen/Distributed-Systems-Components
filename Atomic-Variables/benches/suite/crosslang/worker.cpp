// C++ worker for the cross-language benchmark, via the C ABI (atomvar.h).
//   worker_cpp <arena> <var> <n> <go-file>
// Prints "ready", waits for <go-file>, does n x fetch_add(1), prints a JSON line.
#include "atomvar.h"

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <thread>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc < 5) return 2;
    atomvar_arena *arena = nullptr;
    atomvar_i64 *c = nullptr;
    if (atomvar_arena_open(argv[1], 0, &arena) != ATOMVAR_OK ||
        atomvar_arena_i64(arena, argv[2], 0, &c) != ATOMVAR_OK) {
        std::fprintf(stderr, "atomvar: %s\n", atomvar_last_error());
        return 1;
    }
    long n = std::strtol(argv[3], nullptr, 10);
    std::printf("ready\n");
    std::fflush(stdout);
    while (access(argv[4], F_OK) != 0) std::this_thread::yield();
    int64_t old = 0;
    auto t0 = std::chrono::steady_clock::now();
    for (long i = 0; i < n; i++) atomvar_i64_fetch_add(c, 1, ATOMVAR_SEQ_CST, &old);
    auto ns = std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now() - t0).count();
    std::printf("{\"lang\": \"C++\", \"ops\": %ld, \"elapsed_ns\": %lld}\n", n, (long long)ns);
    atomvar_i64_free(c);
    atomvar_arena_close(arena);
    return 0;
}
