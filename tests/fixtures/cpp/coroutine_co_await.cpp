// m26_coroutine_co_await.cpp - co_await 挂起/恢复语义 (promise + awaiter 协议)
//
// 验证点 (与 g++ -std=c++20 的退出码逐条对照):
//   1) initial_suspend 挂起 => 协程调用 (ramp) 只建帧, 主体一行都不执行;
//   2) promise::await_transform 把 co_await 的操作数转成 awaiter;
//   3) awaiter 三件套 await_ready / await_suspend / await_resume 按协议被调用:
//      await_ready 为真 => 跳过 await_suspend, 但当次仍调用 await_resume 且继续往下跑;
//      await_ready 为假 => 调 await_suspend 并精确挂起, 语句不越过 co_await;
//   4) awaiter 对象跨挂起点存活 (它携带的帧内累加器指针在下次 resume 时仍有效);
//   5) co_return 的值经 return_value 落到帧上, final_suspend 挂起即 done;
//   6) 两个协程函数 (awaitable / second) 共用同一 promise_type: 帧归属必须被正确
//      路由, 两帧交错驱动时状态号/挂起位置互不串扰, 各自的形参与 co_return 值独立.
//
// 观测方式: 每个 awaiter 持一个指向帧内累加器 trace 的指针, 协议钩子往里压入一个
// 十进制位 —— await_transform 压 turn, await_suspend 压 4, await_resume 压 7.
// 于是 trace 就是"事件全序列"指纹, 单个整数即可断言精确执行轨迹.
#include <coroutine>

struct AwaitPromise;

bool await_next(AwaitPromise* p);
int await_result(AwaitPromise* p);
void await_release(AwaitPromise* p);

// 总是挂起的 awaitable: 驱动 initial_suspend / final_suspend 两处挂起点
struct SuspEnd {
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<AwaitPromise> h) noexcept { (void)h; }
    void await_resume() noexcept {}
};

// await_transform 产出的 awaiter: 上一位印记是 turn 2 => 挂起, 否则立即就绪
struct Step {
    long* acc;
    bool await_ready() noexcept { return *acc % 10 != 2; }
    void await_suspend(std::coroutine_handle<AwaitPromise> h) noexcept {
        (void)h;
        *acc = *acc * 10 + 4;
    }
    void await_resume() noexcept { *acc = *acc * 10 + 7; }
};

struct Await {
    typedef AwaitPromise promise_type;
    AwaitPromise* p;
    bool next() { return await_next(p); }
    int value() { return await_result(p); }
    void release() { await_release(p); }
};

struct AwaitPromise {
    int result;
    long trace;
    Await get_return_object() {
        Await a;
        a.p = this;
        result = -1;
        trace = 0;
        return a;
    }
    SuspEnd initial_suspend() { SuspEnd s; return s; }
    SuspEnd final_suspend() noexcept { SuspEnd s; return s; }
    void return_value(int v) { result = v; }
    void unhandled_exception() {}
    Step await_transform(int turn) {
        Step s;
        trace = trace * 10 + turn;
        s.acc = &trace;
        return s;
    }
};

Await awaitable() {
    co_await 1;
    co_await 2;
    co_await 3;
    co_return 42;
}

// 与 awaitable 共用 AwaitPromise: 形参进帧, 挂起轨迹 (2 挂起 / 1 就绪) 与前者相反
Await second(int start) {
    co_await start;
    co_await 1;
    co_return 9;
}

bool await_next(AwaitPromise* p) {
    std::coroutine_handle<AwaitPromise> h = std::coroutine_handle<AwaitPromise>::from_promise(*p);
    if (h.done()) return false;
    h.resume();
    return !h.done();
}

int await_result(AwaitPromise* p) { return p->result; }

void await_release(AwaitPromise* p) {
    std::coroutine_handle<AwaitPromise> h = std::coroutine_handle<AwaitPromise>::from_promise(*p);
    h.destroy();
}

int main() {
    Await t = awaitable();
    Await u = second(2);
    // ramp: initial_suspend 挂起, 两个主体都未执行
    if (t.p->trace != 0) return 1;
    if (u.p->trace != 0) return 2;
    if (t.value() != -1) return 3;
    if (u.value() != -1) return 4;
    // 驱动 A 第一次: transform(1) 就绪 -> resume(1) -> transform(2) 挂起 => 1724
    if (!t.next()) return 5;
    if (t.p->trace != 1724) return 6;
    if (u.p->trace != 0) return 7;
    // 驱动 B 第一次: transform(2) 挂起 => 24; A 不受影响
    if (!u.next()) return 8;
    if (u.p->trace != 24) return 9;
    if (t.p->trace != 1724) return 10;
    if (u.value() != -1) return 11;
    // 再驱动 A 一次: 从 co_await 2 之后精确恢复 -> resume(2) -> transform(3) 就绪
    //                -> resume(3) -> co_return 42 -> final_suspend 挂起 (done)
    if (t.next()) return 12;
    if (t.p->trace != 1724737) return 13;
    if (t.value() != 42) return 14;
    // B 尚未完成: A 的 done 不能污染 B 的状态机
    if (u.next()) return 15;
    if (u.p->trace != 24717) return 16;
    if (u.value() != 9) return 17;
    // 终态后重复驱动不再执行主体
    if (t.next()) return 18;
    if (t.p->trace != 1724737) return 19;
    if (u.next()) return 20;
    if (u.p->trace != 24717) return 21;
    t.release();
    u.release();
    return 0;
}
