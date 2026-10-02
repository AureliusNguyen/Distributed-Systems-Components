// Go worker for the cross-language benchmark, via cgo + the C ABI (atomvar.h).
//
//	worker_go <arena> <var> <n> <go-file>
//
// Prints "ready", waits for <go-file>, does n x fetch_add(1), prints a JSON line.
// Build flags (include/lib paths) are supplied through CGO_CFLAGS / CGO_LDFLAGS.
package main

/*
#cgo LDFLAGS: -latomvar
#include <stdlib.h>
#include "atomvar.h"
*/
import "C"

import (
	"fmt"
	"os"
	"runtime"
	"strconv"
	"time"
	"unsafe"
)

func main() {
	if len(os.Args) < 5 {
		os.Exit(2)
	}
	arenaName := C.CString(os.Args[1])
	varName := C.CString(os.Args[2])
	defer C.free(unsafe.Pointer(arenaName))
	defer C.free(unsafe.Pointer(varName))
	n, _ := strconv.Atoi(os.Args[3])

	var arena *C.atomvar_arena
	var c *C.atomvar_i64
	if C.atomvar_arena_open(arenaName, 0, &arena) != C.ATOMVAR_OK ||
		C.atomvar_arena_i64(arena, varName, 0, &c) != C.ATOMVAR_OK {
		fmt.Fprintln(os.Stderr, "atomvar:", C.GoString(C.atomvar_last_error()))
		os.Exit(1)
	}
	fmt.Println("ready")
	for {
		if _, err := os.Stat(os.Args[4]); err == nil {
			break
		}
		runtime.Gosched()
	}
	var old C.int64_t
	t0 := time.Now()
	for i := 0; i < n; i++ {
		C.atomvar_i64_fetch_add(c, 1, C.ATOMVAR_SEQ_CST, &old)
	}
	el := time.Since(t0).Nanoseconds()
	fmt.Printf("{\"lang\": \"Go\", \"ops\": %d, \"elapsed_ns\": %d}\n", n, el)
	C.atomvar_i64_free(c)
	C.atomvar_arena_close(arena)
}
