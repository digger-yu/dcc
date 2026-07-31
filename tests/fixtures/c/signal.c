// signal/sigaction/sigaltstack libc forwarding fixture (Section 11 P0.2).
// 注: dcc 当前 struct sigaction / stack_t 布局有 bug (Section 9 follow-up),
// 所以 fixture 只测运行时安全的 signal(); sigaction/sigaltstack 通过
// 集成测试 verify .o 引用 + 可成功 compile (不在此 fixture 运行).
#include <signal.h>

static int handler_calls;

static void handler(int sig) {
    (void)sig;
    handler_calls++;
}

int main(void) {
    // signal(SIGTERM, handler): 旧 handler 通常为 SIG_DFL (= 0).
    void (*prev)(int) = signal(15, handler);
    // 旧 handler 不为 NULL (因为是 SIG_DFL 或其他) → prev != handler.
    if (prev == handler) return 1;
    (void)prev;

    // 再设回去, 恢复默认 (避免后续干扰).
    signal(15, (void (*)(int))0);

    return 0;
}
