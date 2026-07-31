// C23: thread_local 关键字 (C23 拼写, 等价 C11 的 _Thread_local)
#include <stdio.h>

thread_local int tls_g = 42;
thread_local static int tls_s = 7;

static thread_local double tls_d = 1.5;

int main(void) {
    // 局部 thread_local
    thread_local int local_tls = 3;
    if (local_tls != 3) return 1;
    if (tls_g != 42) return 2;
    if (tls_s != 7) return 3;

    // TLS 变量可写且独立于普通全局
    tls_g += 1;
    if (tls_g != 43) return 4;

    // 浮点 TLS
    if (tls_d != 1.5) return 5;

    // 指针 TLS
    thread_local int *p = 0;
    if (p != 0) return 6;

    printf("OK tls_g=%d tls_s=%d local=%d tls_d=%.1f\n",
           tls_g, tls_s, local_tls, tls_d);
    return 0;
}
