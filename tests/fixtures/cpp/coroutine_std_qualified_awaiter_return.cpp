// coroutine_std_qualified_awaiter_return.cpp —— 协程 promise 用**标准形式**的
// 限定名 awaiter 返回类型 ([dcl.fct]/[class.mfct], 不借道 typedef 绕法).
//
// 背景: <coroutine> 桩头与 coroutine_lower 的注释都记载"C++ 解析器不能在类内成员
// 函数签名上接受限定名返回类型 (`std::suspend_always initial_suspend()` 退化为
// Opaque)", 因此要求 promise 写成 `typedef std::suspend_always Susp;` 后返回
// `Susp`. 该限制已被解除: 类内成员函数 / 命名空间作用域自由函数都可以直接用
// `std::suspend_always` 作返回类型. 本夹具锁住这条标准形式, 并顺带跑通
// lazy generator 的完整协议 (initial/final suspend + yield_value + resume 驱动).
//
// 注意 awaiter 的构造写法: `return {};` 而非 `return std::suspend_always();` ——
// 后者是纯聚合体的 value-init, 会引用未合成的隐式默认构造定义 (链接期报
// `suspend_always_ctor_complete` 未定义), 与限定名解析无关, 属另一处归属域缺口.
//
// 验证点 (退出码与 stdout 逐条与 g++ -std=c++20 真值对照).
#include <coroutine>

struct GenPromise;

bool coro_next(GenPromise* p);
int coro_value(GenPromise* p);

struct Generator {
    typedef GenPromise promise_type;
    GenPromise* p;
    bool next() { return coro_next(p); }
    int value() { return coro_value(p); }
};

struct GenPromise {
    int yielded;
    Generator get_return_object() {
        Generator g;
        g.p = this;
        yielded = 0;
        return g;
    }
    // 标准形式: 类内成员函数签名上用限定名返回 awaiter, 不写 typedef 别名.
    std::suspend_always initial_suspend() { return {}; }
    std::suspend_always final_suspend() noexcept { return {}; }
    std::suspend_always yield_value(int v) {
        yielded = v;
        return {};
    }
    void return_void() {}
    void unhandled_exception() {}
};

// 命名空间作用域的自由函数同样能用限定名作返回类型.
std::suspend_always make_always() { return {}; }

int coro_value(GenPromise* p) { return p->yielded; }

Generator count_up() {
    co_yield 1;
    co_yield 2;
    co_yield 3;
    co_return;
}

bool coro_next(GenPromise* p) {
    std::coroutine_handle<GenPromise> h =
        std::coroutine_handle<GenPromise>::from_promise(*p);
    if (h.done()) return false;
    h.resume();
    return !h.done();
}

int main() {
    // 命名空间作用域的限定名返回类型能用 (证明不是只"解析通过", 而是真参与调用).
    make_always();

    Generator g = count_up();
    // lazy: 停在 initial_suspend, 尚未产出
    if (g.value() != 0) return 1;
    if (!g.next()) return 2;
    if (g.value() != 1) return 3;
    if (!g.next()) return 4;
    if (g.value() != 2) return 5;
    if (!g.next()) return 6;
    if (g.value() != 3) return 7;
    // co_return 后停在 final_suspend => 耗尽
    if (g.next()) return 8;
    if (g.value() != 3) return 9;
    return 0;
}
