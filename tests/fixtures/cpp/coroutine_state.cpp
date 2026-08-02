// m26_coroutine_state.cpp - 协程真状态机 smoke test
// 验证: co_yield/co_return 降级为 Duff's device 状态机
// (switch(<fn>_coro_state) { case 0: ... case N: ... }),
// per-function 状态变量 <fn>_coro_state / <fn>_coro_done.
// 注: dcc C 后端只接受 case 标签位于 switch 直接子级,
// 故挂起点位于函数体顶层 (嵌套控制流内挂起为 dcc C 后端限制).

void f() {
    co_yield 1;
    co_yield 2;
    co_return;
}

int main() {
    f();
    return 0;
}
