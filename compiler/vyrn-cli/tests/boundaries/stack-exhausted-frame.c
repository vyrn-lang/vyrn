/* Links before a native host built from `stack-exhausted.vyrn`, compiled with
   `-Dmain=vyrn_host_main`. Its first exception handler takes 12 KiB of the
   overflowed stack before wasm-rt's trap runs, as an exception frame that
   holds an AMX context does. */
#undef main
#include "wasm-rt-impl.h"
#include <malloc.h>
#include <windows.h>

int vyrn_host_main(int argc, char** argv);

static LONG wide_frame(PEXCEPTION_POINTERS info) {
    if (info->ExceptionRecord->ExceptionCode != EXCEPTION_STACK_OVERFLOW) {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    volatile char* frame = alloca(12 * 1024);
    for (int i = 12 * 1024 - 1; i >= 0; i -= 512) frame[i] = 0;
    frame[0] = 0;
    wasm_rt_trap(WASM_RT_TRAP_EXHAUSTION);
    return EXCEPTION_CONTINUE_SEARCH;
}

/* wasm-rt installs its handler once, so the host's own `wasm_rt_init` keeps
   this handler first. */
int main(int argc, char** argv) {
    wasm_rt_init();
    AddVectoredExceptionHandler(1, wide_frame);
    return vyrn_host_main(argc, argv);
}
