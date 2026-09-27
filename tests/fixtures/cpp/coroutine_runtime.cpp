// coroutine_runtime.cpp —— co_yield / co_return 降级为状态机后运行时保持挂起位置
//
// 验证点 (退出码逐条与 g++ -std=c++20 对照):
//   1) 协程形参 (n) 与函数内局部量 (i / total) 被提升进协程帧, 跨挂起点存活;
//   2) 挂起点位于 while 循环体内: 回边 (loop back-edge) 与挂起共存, 每次 next()
//      恰好推进一个 co_yield;
//   3) 挂起点位于 for 循环体内且循环变量声明在 for-init 里 (块作用域局部量提升);
//   4) 未成立的 if 分支里的 co_yield 也要正确接线 (死路径不能污染状态机);
//   5) 每个协程调用有独立的帧: 两个 Generator 交错推进互不干扰;
//   6) co_return 后 final_suspend 挂起 => done() 为真, 重复驱动不再执行主体.
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

Generator counter(int n) {
    int i = 1;
    int total = 0;
    while (i <= n) {
        total = total + i;
        co_yield i;
        i = i + 1;
    }
    for (int k = 0; k < 1; k = k + 1) {
        co_yield -total;
    }
    if (n < 0) {
        // 死路径: n 恒为正, 但该挂起点仍必须被编进状态机
        co_yield 1000;
    }
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
    Generator a = counter(3);
    Generator b = counter(1);
    // 都停在 initial_suspend 之后, 尚未产出
    if (a.value() != 0) return 1;
    if (b.value() != 0) return 2;
    if (!a.next()) return 3;
    if (a.value() != 1) return 4;
    if (!b.next()) return 5;
    if (b.value() != 1) return 6;   // b 的挂起位置独立于 a
    if (!a.next()) return 7;
    if (a.value() != 2) return 8;
    if (!b.next()) return 9;
    if (b.value() != -1) return 10; // b 的 while 结束, 进 for 臂产出 -total
    if (!a.next()) return 11;
    if (a.value() != 3) return 12;
    if (b.next()) return 13;        // b 已 co_return
    if (b.value() != -1) return 14;
    if (!a.next()) return 15;
    if (a.value() != -6) return 16; // a 的 while 累加 1+2+3=6, for 臂产出 -6
    if (a.next()) return 17;        // a 走完 for 与 if 后 co_return
    if (a.value() != -6) return 18;
    if (b.next()) return 19;        // 终态后重复驱动仍为 false
    if (a.next()) return 20;
    a.release();
    b.release();
    return 0;
}
