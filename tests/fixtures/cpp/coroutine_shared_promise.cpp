// m52_coroutine_shared_promise.cpp - 三个协程函数共用同一 promise_type 的帧归属
//
// 验证点 (与 g++ -std=c++20 的退出码逐条对照):
//   1) 同一 promise_type 被 a(int) / b(int) / c() 三个协程函数共用时, 每次
//      handle::resume() 必须路由回**该帧自己的**状态机 (帧归属标签 dcc_coro_fn);
//   2) 三台状态机各自编号/各自提升形参与局部量, 轮转驱动 30 轮互不串扰:
//      每个帧的产出序列 (最后一枚 co_yield 值 + 成功推进次数 + 累加指纹) 都独立正确;
//   3) 带循环回边的机器 (a 含 while+for, b 含 while) 与小机器 (c) 混用同一 promise
//      也不共享挂起位置;
//   4) 任一帧 done 之后重复驱动不再执行其主体.
// 3 个协程函数共用同一 promise_type, 其中 2 个带循环 (各自 >=10 臂), 交错驱动
#include <coroutine>

struct GP;
bool nxt(GP* p);
int val(GP* p);
void rel(GP* p);

struct SE {
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<GP> h) noexcept { (void)h; }
    void await_resume() noexcept {}
};
struct Gen {
    typedef GP promise_type;
    GP* p;
    bool next() { return nxt(p); }
    int value() { return val(p); }
    void release() { rel(p); }
};
struct GP {
    int y;
    long fingerprint;
    Gen get_return_object() { Gen g; g.p = this; y = 0; fingerprint = 0; return g; }
    SE initial_suspend() { SE a; return a; }
    SE final_suspend() noexcept { SE a; return a; }
    SE yield_value(int v) { y = v; fingerprint = fingerprint * 3 + v; SE a; return a; }
    void return_void() {}
    void unhandled_exception() {}
};

Gen a(int n) {
    int i = 1;
    int sum = 0;
    while (i <= n) { sum = sum + i; co_yield sum; i = i + 1; }
    for (int k = 0; k < 2; k = k + 1) { co_yield -sum; }
    co_return;
}
Gen b(int n) {
    int j = 0;
    while (j < n) { j = j + 3; co_yield j * 2; }
    co_yield 999;
    co_return;
}
Gen c() { co_yield 7; co_yield 8; co_return; }

bool nxt(GP* p) {
    std::coroutine_handle<GP> h = std::coroutine_handle<GP>::from_promise(*p);
    if (h.done()) return false;
    h.resume();
    return !h.done();
}
int val(GP* p) { return p->y; }
void rel(GP* p) {
    std::coroutine_handle<GP> h = std::coroutine_handle<GP>::from_promise(*p);
    h.destroy();
}

int main() {
    Gen x = a(4);
    Gen y = b(3);
    Gen z = c();
    int cx = 0;
    int cy = 0;
    int cz = 0;
    for (int i = 0; i < 30; i = i + 1) {
        if (x.next()) { cx = cx + 1; }
        if (y.next()) { cy = cy + 1; }
        if (z.next()) { cz = cz + 1; }
    }
    // 每个帧各自产出完整序列, 轮转驱动不串扰
    if (x.value() != -10) return 1;
    if (y.value() != 999) return 2;
    if (z.value() != 8) return 3;
    if (cx != 6) return 4;
    if (cy != 2) return 5;
    if (cz != 2) return 6;
    if (x.p->fingerprint != 698) return 7;
    if (y.p->fingerprint != 1017) return 8;
    if (z.p->fingerprint != 29) return 9;
    // 终态后重复驱动不再推进
    if (x.next()) return 10;
    if (y.next()) return 11;
    if (z.next()) return 12;
    if (cx != 6) return 13;
    x.release();
    y.release();
    z.release();
    return 0;
}
