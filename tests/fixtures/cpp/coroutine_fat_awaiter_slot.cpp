// coroutine_fat_awaiter_slot.cpp - 协程帧 awaiter 槽 (8/24/32 字节) 跨挂起点的整块存活
//
// 背景: 帧槽里的 awaiter 对象是**唯一**存放处, `co_await` 降级后三个协议钩子
// (`await_ready` / `await_suspend` / `await_resume`) 都必须打在同一个 awaiter 对象上
// ([basic.coro.await]), 而该对象在挂起点之间住在协程帧里 (裸整块复制搬运, 见
// src/cpp/coroutine_lower.rs 模块文档)。两处缺陷都会吃掉 awaiter 自身的状态:
//   · x86_64 后端 "多 quad 聚合值装载漏第二个字" —— >16 字节的整块复制只有前两字落地;
//   · 降级器曾把帧槽拷进 step 函数局部量 `d`, 钩子打在 `d` 上 —— `await_suspend` 的写
//     随臂结束消失 (这一条连 8 字节 awaiter 都中招, 只是既有夹具从不改写 awaiter
//     自身字段, 所以一直没被发现)。
// `coroutine_lower` 当年据此把 awaiter 槽准入限制在 ≤8 字节, 该限制随根因修复解除;
// 本夹具就是它的回归护栏。
//
// 验证方式 (与 g++ -std=c++20 的 stdout 逐字节 + 退出码对照):
//   · 三种尺寸的 awaiter 各建一族: SuspEnd (8B, 驱动 initial/final_suspend)、
//     A24 (3 个 long = 24B)、A32 (4 个 long = 32B, 第 3/4 字落在第二对 quad 上);
//   · `await_transform` 按 `seed*100 + k` 的等差模式装载每一字;
//   · `await_suspend` 改写**非首字** (乘 10 加印记), 使首字之外的写必须经帧槽往返
//     才能存活; `await_resume` 逐字核对模式、打印全部字, 不一致就累加 `bad`;
//   · 三条协程 (其中两条共用 promise, 走不同的分支臂) 交错驱动, 确认一个帧的槽
//     读写不影响另一个帧; 终态后重复驱动不得再执行主体。
// 注: 本夹具刻意不用 C++ 前端尚未支持的写法 (逗号多声明 `long a, b;`、
// `x = co_await y`、`while (a && b)`), 那些是前端域的独立缺陷。
#include <coroutine>
#include <cstdio>

struct FatPromise;
struct BigPromise;

bool fat_next(FatPromise *p);
void fat_release(FatPromise *p);
bool big_next(BigPromise *p);
void big_release(BigPromise *p);

static int bad;

/// 8 字节 awaiter: 旧限制的分界点, 同样要在 await_suspend 里改写自身字段
struct SuspEnd {
    long tag;
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<FatPromise> h) noexcept {
        (void)h;
        tag = tag * 10 + 1;
    }
    void await_resume() noexcept {
        if (tag != 1) {
            bad++;
        }
    }
};

struct SuspEnd32 {
    long tag;
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<BigPromise> h) noexcept {
        (void)h;
        tag = tag * 10 + 1;
    }
    void await_resume() noexcept {
        if (tag != 1) {
            bad++;
        }
    }
};

/// 24 字节 awaiter: 三个字都要跨挂起点存活, 末字由 await_suspend 改写
struct A24 {
    long w0;
    long w1;
    long w2;
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<FatPromise> h) noexcept {
        (void)h;
        w2 = w2 * 10 + 4;
    }
    void await_resume() noexcept {
        if (w1 != w0 + 1) {
            bad++;
        }
        if (w2 != (w0 + 2) * 10 + 4) {
            bad++;
        }
        printf("A24 %ld %ld %ld\n", w0, w1, w2);
    }
};

/// 32 字节 awaiter: 第 3/4 字落在整块搬运的第二对 quad 上, 是本轮缺陷的靶心
struct A32 {
    long w0;
    long w1;
    long w2;
    long w3;
    bool await_ready() noexcept { return false; }
    void await_suspend(std::coroutine_handle<BigPromise> h) noexcept {
        (void)h;
        w2 = w2 * 10 + 2;
        w3 = w3 * 10 + 3;
    }
    void await_resume() noexcept {
        if (w1 != w0 + 1) {
            bad++;
        }
        if (w2 != (w0 + 2) * 10 + 2) {
            bad++;
        }
        if (w3 != (w0 + 3) * 10 + 3) {
            bad++;
        }
        printf("A32 %ld %ld %ld %ld\n", w0, w1, w2, w3);
    }
};

struct Fat {
    typedef FatPromise promise_type;
    FatPromise *p;
    bool next() { return fat_next(p); }
    void release() { fat_release(p); }
};

struct Big {
    typedef BigPromise promise_type;
    BigPromise *p;
    bool next() { return big_next(p); }
    void release() { big_release(p); }
};

struct FatPromise {
    int result;
    Fat get_return_object() {
        Fat f;
        f.p = this;
        result = -1;
        return f;
    }
    SuspEnd initial_suspend() { SuspEnd s; s.tag = 0; return s; }
    SuspEnd final_suspend() noexcept { SuspEnd s; s.tag = 0; return s; }
    void return_value(int v) { result = v; }
    void unhandled_exception() {}
    A24 await_transform(int seed) {
        A24 a;
        a.w0 = (long)seed * 100;
        a.w1 = a.w0 + 1;
        a.w2 = a.w0 + 2;
        return a;
    }
};

struct BigPromise {
    int result;
    Big get_return_object() {
        Big f;
        f.p = this;
        result = -1;
        return f;
    }
    SuspEnd32 initial_suspend() { SuspEnd32 s; s.tag = 0; return s; }
    SuspEnd32 final_suspend() noexcept { SuspEnd32 s; s.tag = 0; return s; }
    void return_value(int v) { result = v; }
    void unhandled_exception() {}
    A32 await_transform(int seed) {
        A32 a;
        a.w0 = (long)seed * 100;
        a.w1 = a.w0 + 1;
        a.w2 = a.w0 + 2;
        a.w3 = a.w0 + 3;
        return a;
    }
};

/// 三个挂起点, 第三个走条件分支臂 (局部量提升 + Branch 臂)
Fat fat_await() {
    int step;
    step = 0;
    co_await 1;
    step = step + 1;
    co_await 2;
    step = step + 1;
    if (step > 1) {
        co_await 3;
    }
    co_return 71;
}

/// 与 fat_await 共用 promise: 分支不成立 => 只有一个体内挂起点, 两帧互不串扰
Fat fat_second() {
    int step;
    step = 0;
    co_await 4;
    step = step + 2;
    if (step > 3) {
        co_await 5;
    }
    co_return 82;
}

/// 32 字节 awaiter 族: 两个体内挂起点
Big big_await() {
    co_await 6;
    co_await 7;
    co_return 93;
}

bool fat_next(FatPromise *p) {
    std::coroutine_handle<FatPromise> h = std::coroutine_handle<FatPromise>::from_promise(*p);
    if (h.done()) {
        return false;
    }
    h.resume();
    return !h.done();
}

void fat_release(FatPromise *p) {
    std::coroutine_handle<FatPromise> h = std::coroutine_handle<FatPromise>::from_promise(*p);
    h.destroy();
}

bool big_next(BigPromise *p) {
    std::coroutine_handle<BigPromise> h = std::coroutine_handle<BigPromise>::from_promise(*p);
    if (h.done()) {
        return false;
    }
    h.resume();
    return !h.done();
}

void big_release(BigPromise *p) {
    std::coroutine_handle<BigPromise> h = std::coroutine_handle<BigPromise>::from_promise(*p);
    h.destroy();
}

int main() {
    Fat a = fat_await();
    Fat b = fat_second();
    Big c = big_await();
    // ramp 只建帧: 三个主体一行都没执行
    if (a.p->result != -1) {
        return 20;
    }
    if (b.p->result != -1) {
        return 21;
    }
    if (c.p->result != -1) {
        return 22;
    }
    // 交错驱动到终态 (a 需 4 次, b/c 各需 3 次, 多余的一次次调用应返回 false 且无输出)
    if (a.next()) {
        printf("a1\n");
    }
    if (b.next()) {
        printf("b1\n");
    }
    if (c.next()) {
        printf("c1\n");
    }
    if (a.next()) {
        printf("a2\n");
    }
    if (b.next()) {
        printf("b2\n");
    }
    if (c.next()) {
        printf("c2\n");
    }
    if (a.next()) {
        printf("a3\n");
    }
    if (b.next()) {
        printf("b3\n");
    }
    if (c.next()) {
        printf("c3\n");
    }
    if (a.next()) {
        printf("a4\n");
    }
    if (b.next()) {
        printf("b4\n");
    }
    if (c.next()) {
        printf("c4\n");
    }
    printf("res %d %d %d\n", a.p->result, b.p->result, c.p->result);
    // 终态后重复驱动不得再执行主体 (再打印一行就会改变 stdout)
    if (a.next()) {
        return 23;
    }
    if (b.next()) {
        return 24;
    }
    if (c.next()) {
        return 25;
    }
    printf("bad=%d\n", bad);
    a.release();
    b.release();
    c.release();
    return bad;
}
