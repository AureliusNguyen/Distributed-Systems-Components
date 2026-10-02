/* Exercises every function in atomvar.h. Built and run by tests/c_abi.rs. */
#include "atomvar.h"

#include <inttypes.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;

#define CHECK(cond)                                                            \
    do {                                                                       \
        if (!(cond)) {                                                         \
            fprintf(stderr, "FAIL %s:%d: %s (last_error: %s)\n", __FILE__,     \
                    __LINE__, #cond, atomvar_last_error());                    \
            failures++;                                                        \
        }                                                                      \
    } while (0)

#define OK(call) CHECK((call) == ATOMVAR_OK)
#define SC ATOMVAR_SEQ_CST

static void test_i64(const atomvar_arena *arena) {
    atomvar_i64 *h = NULL, *h2 = NULL;
    int64_t v = 0, prev = 0;
    bool ex = false;

    OK(atomvar_i64_new(INT64_MAX, &h));
    OK(atomvar_i64_fetch_add(h, 1, SC, &v));
    CHECK(v == INT64_MAX);
    OK(atomvar_i64_load(h, ATOMVAR_ACQUIRE, &v));
    CHECK(v == INT64_MIN); /* wraps */
    OK(atomvar_i64_store(h, 10, ATOMVAR_RELEASE));
    OK(atomvar_i64_swap(h, 20, SC, &v));
    CHECK(v == 10);
    OK(atomvar_i64_compare_exchange(h, 20, 30, SC, SC, &prev, &ex));
    CHECK(ex && prev == 20);
    OK(atomvar_i64_compare_exchange(h, 20, 40, SC, SC, &prev, &ex));
    CHECK(!ex && prev == 30);
    do {
        OK(atomvar_i64_compare_exchange_weak(h, 30, 31, SC, SC, &prev, &ex));
    } while (!ex && prev == 30);
    CHECK(ex);
    OK(atomvar_i64_fetch_sub(h, 1, SC, &v));
    CHECK(v == 31);
    OK(atomvar_i64_fetch_and(h, 0x1c, SC, &v));
    OK(atomvar_i64_fetch_or(h, 1, SC, &v));
    OK(atomvar_i64_fetch_xor(h, 3, SC, &v));
    OK(atomvar_i64_load(h, SC, &v));
    CHECK(v == (((30 & 0x1c) | 1) ^ 3));
    OK(atomvar_i64_fetch_max(h, 100, SC, &v));
    OK(atomvar_i64_fetch_min(h, -5, SC, &v));
    CHECK(v == 100);
    OK(atomvar_i64_add_and_get(h, 10, SC, &v));
    CHECK(v == 5);
    OK(atomvar_i64_sub_and_get(h, 6, SC, &v));
    CHECK(v == -1);
    /* invalid orderings */
    CHECK(atomvar_i64_load(h, ATOMVAR_RELEASE, &v) == ATOMVAR_E_INVALID_ORDERING);
    CHECK(strlen(atomvar_last_error()) > 0);
    CHECK(atomvar_i64_store(h, 1, ATOMVAR_ACQUIRE) == ATOMVAR_E_INVALID_ORDERING);
    CHECK(atomvar_i64_compare_exchange(h, 0, 1, ATOMVAR_RELAXED, SC, &prev, &ex) ==
          ATOMVAR_E_INVALID_ORDERING);
    CHECK(atomvar_i64_load(h, 99, &v) == ATOMVAR_E_INVALID_ORDERING);
    /* NULL out-pointer */
    CHECK(atomvar_i64_load(h, SC, NULL) == ATOMVAR_E_INVALID_ARGUMENT);
    atomvar_i64_free(h);
    atomvar_i64_free(NULL);

    /* arena-backed */
    OK(atomvar_arena_i64(arena, "i", 5, &h));
    OK(atomvar_arena_open_i64(arena, "i", &h2));
    OK(atomvar_i64_add_and_get(h2, 1, SC, &v));
    CHECK(v == 6);
    OK(atomvar_i64_load(h, SC, &v));
    CHECK(v == 6);
    CHECK(atomvar_arena_open_i64(arena, "missing", &h2) == ATOMVAR_E_NOT_FOUND);
    atomvar_i64_free(h);
}

static void test_u64(const atomvar_arena *arena) {
    atomvar_u64 *h = NULL, *h2 = NULL;
    uint64_t v = 0, prev = 0;
    bool ex = false;
    OK(atomvar_u64_new(UINT64_MAX, &h));
    OK(atomvar_u64_add_and_get(h, 1, SC, &v));
    CHECK(v == 0);
    OK(atomvar_u64_sub_and_get(h, 1, SC, &v));
    CHECK(v == UINT64_MAX);
    OK(atomvar_u64_store(h, 8, SC));
    OK(atomvar_u64_swap(h, 9, SC, &v));
    CHECK(v == 8);
    OK(atomvar_u64_compare_exchange(h, 9, 12, SC, ATOMVAR_RELAXED, &prev, &ex));
    CHECK(ex && prev == 9);
    do {
        OK(atomvar_u64_compare_exchange_weak(h, 12, 13, SC, SC, &prev, &ex));
    } while (!ex && prev == 12);
    OK(atomvar_u64_fetch_add(h, 2, SC, &v));
    CHECK(v == 13);
    OK(atomvar_u64_fetch_sub(h, 5, SC, &v));
    OK(atomvar_u64_fetch_and(h, 0xff, SC, &v));
    OK(atomvar_u64_fetch_or(h, 0x100, SC, &v));
    OK(atomvar_u64_fetch_xor(h, 0x1, SC, &v));
    OK(atomvar_u64_fetch_max(h, 1000, SC, &v));
    OK(atomvar_u64_fetch_min(h, 3, SC, &v));
    CHECK(v == 1000);
    OK(atomvar_u64_load(h, SC, &v));
    CHECK(v == 3);
    atomvar_u64_free(h);

    OK(atomvar_arena_u64(arena, "u", 1, &h));
    OK(atomvar_arena_open_u64(arena, "u", &h2));
    atomvar_u64_free(h);
    atomvar_u64_free(h2);
    /* type mismatch: "i" is an i64 */
    CHECK(atomvar_arena_u64(arena, "i", 0, &h) == ATOMVAR_E_TYPE_MISMATCH);
}

static void test_bool(const atomvar_arena *arena) {
    atomvar_bool *h = NULL, *h2 = NULL;
    bool v = false, prev = false, ex = false;
    OK(atomvar_bool_new(false, &h));
    OK(atomvar_bool_fetch_or(h, true, SC, &v));
    CHECK(v == false);
    OK(atomvar_bool_fetch_and(h, true, SC, &v));
    CHECK(v == true);
    OK(atomvar_bool_fetch_xor(h, true, SC, &v));
    OK(atomvar_bool_fetch_nand(h, true, SC, &v));
    CHECK(v == false);
    OK(atomvar_bool_load(h, SC, &v));
    CHECK(v == true);
    OK(atomvar_bool_store(h, false, SC));
    OK(atomvar_bool_swap(h, true, SC, &v));
    CHECK(v == false);
    OK(atomvar_bool_compare_exchange(h, true, false, SC, SC, &prev, &ex));
    CHECK(ex && prev == true);
    do {
        OK(atomvar_bool_compare_exchange_weak(h, false, true, SC, SC, &prev, &ex));
    } while (!ex && prev == false);
    atomvar_bool_free(h);
    OK(atomvar_arena_bool(arena, "b", true, &h));
    OK(atomvar_arena_open_bool(arena, "b", &h2));
    OK(atomvar_bool_load(h2, SC, &v));
    CHECK(v == true);
    atomvar_bool_free(h);
    atomvar_bool_free(h2);
}

static void test_f64(const atomvar_arena *arena) {
    atomvar_f64 *h = NULL, *h2 = NULL;
    double v = 0, prev = 0;
    uint64_t bits = 0, pbits = 0;
    bool ex = false;
    OK(atomvar_f64_new(1.5, &h));
    OK(atomvar_f64_fetch_add(h, 2.0, SC, &v));
    CHECK(v == 1.5);
    OK(atomvar_f64_fetch_sub(h, 0.5, SC, &v));
    CHECK(v == 3.5);
    OK(atomvar_f64_add_and_get(h, 1.0, SC, &v));
    CHECK(v == 4.0);
    OK(atomvar_f64_store(h, 0.0, SC));
    /* bitwise CAS: -0.0 does not match +0.0 */
    OK(atomvar_f64_compare_exchange(h, -0.0, 1.0, SC, SC, &prev, &ex));
    CHECK(!ex);
    OK(atomvar_f64_compare_exchange(h, 0.0, -0.0, SC, SC, &prev, &ex));
    CHECK(ex);
    OK(atomvar_f64_load_bits(h, SC, &bits));
    CHECK(bits == 0x8000000000000000ULL);
    OK(atomvar_f64_store_bits(h, 0x7ff8000000000001ULL, SC));
    OK(atomvar_f64_load(h, SC, &v));
    CHECK(isnan(v));
    OK(atomvar_f64_compare_exchange_bits(h, 0x7ff8000000000002ULL, 0, SC, SC, &pbits, &ex));
    CHECK(!ex && pbits == 0x7ff8000000000001ULL);
    OK(atomvar_f64_compare_exchange_bits(h, 0x7ff8000000000001ULL, 0, SC, SC, &pbits, &ex));
    CHECK(ex);
    OK(atomvar_f64_swap(h, 2.25, SC, &v));
    CHECK(v == 0.0);
    atomvar_f64_free(h);
    OK(atomvar_arena_f64(arena, "f", 0.5, &h));
    OK(atomvar_arena_open_f64(arena, "f", &h2));
    OK(atomvar_f64_load(h2, SC, &v));
    CHECK(v == 0.5);
    atomvar_f64_free(h);
    atomvar_f64_free(h2);
}

static void test_u128(const atomvar_arena *arena) {
    atomvar_u128 *h = NULL, *h2 = NULL;
    atomvar_uint128 v = {0, 0}, prev = {0, 0};
    atomvar_uint128 max = {UINT64_MAX, UINT64_MAX};
    atomvar_uint128 tagged = {42, 7};
    bool ex = false;
    OK(atomvar_u128_new(max, &h));
    OK(atomvar_u128_load(h, SC, &v));
    CHECK(v.lo == UINT64_MAX && v.hi == UINT64_MAX);
    OK(atomvar_u128_compare_exchange(h, max, tagged, SC, SC, &prev, &ex));
    CHECK(ex);
    OK(atomvar_u128_load(h, SC, &v));
    CHECK(v.lo == 42 && v.hi == 7);
    do {
        OK(atomvar_u128_compare_exchange_weak(h, tagged, max, SC, SC, &prev, &ex));
    } while (!ex && prev.lo == 42 && prev.hi == 7);
    OK(atomvar_u128_store(h, tagged, SC));
    OK(atomvar_u128_swap(h, max, SC, &v));
    CHECK(v.lo == 42 && v.hi == 7);
    atomvar_u128_free(h);
    OK(atomvar_arena_u128(arena, "w", tagged, &h));
    OK(atomvar_arena_open_u128(arena, "w", &h2));
    OK(atomvar_u128_load(h2, SC, &v));
    CHECK(v.lo == 42 && v.hi == 7);
    atomvar_u128_free(h);
    atomvar_u128_free(h2);
}

int main(int argc, char **argv) {
    atomvar_arena *arena = NULL, *again = NULL;
    if (argc < 2) {
        fprintf(stderr, "usage: ffi_test <arena-name>\n");
        return 2;
    }
    OK(atomvar_init());
    OK(atomvar_arena_open(argv[1], 16, &arena));
    CHECK(atomvar_arena_capacity(arena) == 16);
    CHECK(atomvar_arena_open(argv[1], 32, &again) == ATOMVAR_E_LAYOUT_MISMATCH);
    CHECK(atomvar_arena_open("bad/name", 0, &again) == ATOMVAR_E_INVALID_NAME);
    CHECK(atomvar_arena_open(NULL, 0, &again) == ATOMVAR_E_INVALID_ARGUMENT);

    test_i64(arena);
    test_u64(arena);
    test_bool(arena);
    test_f64(arena);
    test_u128(arena);

    atomvar_arena_close(arena);
    atomvar_arena_close(NULL);
    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    printf("ALL OK\n");
    return 0;
}
