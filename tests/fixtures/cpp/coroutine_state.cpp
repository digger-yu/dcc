// coroutine_state.cpp —— C++20 协程 co_yield / co_return 的状态机降级 (语法 + promise/awaiter 协议)
//
// 验证点 (退出码逐条与 g++ -std=c++20 对照):
//   1) co_yield / co_return 语法解析 + promise_type 协议 (get_return_object /
//      initial_suspend / final_suspend / yield_value / return_void / unhandled_exception);
//   2) std::coroutine_handle<P> 的 from_promise / done / resume / destroy 协议;
//   3) 挂起位置跨 resume 调用保持 (initial_suspend -> 第 1 个 co_yield -> 第 2 个
//      co_yield -> co_return 后的 final_suspend), 每次 next() 恰好推进一个挂起点;
//   4) 协程停在 final_suspend 时 done() 为真, 之后重复驱动不再执行协程体;
//   5) 每次调用协程函数得到独立的帧 (两个实例互不干扰).
#include <coroutine>

struct GenPromise;

// 总是挂起的 awaitable: 驱动 initial_suspend / co_yield / final_suspend 三处挂起点
struct SuspEnd {
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<GenPromise> h) noexcept { (void)h; }
    void await_resume() noexcept {}
};

bool coro_next(GenPromise* p);
int coro_value(GenPromise* p);
void coro_release(GenPromise* p);

struct Generator {
    typedef GenPromise promise_type;
    GenPromise* p;
    bool next() { return coro_next(p); }
    int value() { return coro_value(p); }
    void release() { coro_release(p); }
};

struct GenPromise {
    int yielded;
    Generator get_return_object() {
        Generator g;
        g.p = this;
        yielded = 0;
        return g;
    }
    SuspEnd initial_suspend() { SuspEnd a; return a; }
    SuspEnd final_suspend() noexcept { SuspEnd a; return a; }
    SuspEnd yield_value(int v) { yielded = v; SuspEnd a; return a; }
    void return_void() {}
    void unhandled_exception() {}
};

Generator f() {
    co_yield 1;
    co_yield 2;
    co_return;
}

bool coro_next(GenPromise* p) {
    std::coroutine_handle<GenPromise> h = std::coroutine_handle<GenPromise>::from_promise(*p);
    if (h.done()) return false;
    h.resume();
    return !h.done();
}

int coro_value(GenPromise* p) { return p->yielded; }

void coro_release(GenPromise* p) {
    std::coroutine_handle<GenPromise> h = std::coroutine_handle<GenPromise>::from_promise(*p);
    h.destroy();
}

int main() {
    Generator g = f();
    // ramp 之后停在 initial_suspend: 协程体一行都没跑
    if (g.value() != 0) return 1;
    if (!g.next()) return 2;
    if (g.value() != 1) return 3;
    if (!g.next()) return 4;
    if (g.value() != 2) return 5;
    // 两个 co_yield 之后是 co_return => final_suspend 挂起 => done()
    if (g.next()) return 6;
    if (g.value() != 2) return 7;
    // 终态后重复驱动不再执行协程体
    if (g.next()) return 8;
    if (g.value() != 2) return 9;
    g.release();
    // 第二次调用协程函数得到全新的帧
    Generator h = f();
    if (h.value() != 0) return 10;
    if (!h.next()) return 11;
    if (h.value() != 1) return 12;
    h.release();
    return 0;
}
