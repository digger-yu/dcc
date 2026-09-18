// m26_coroutine_state.cpp - 协程状态机 smoke test
// ⚠️ Phase 1: compile-only test (runtime support pending)
// 验证：co_yield/co_return 语法解析正确性
// 
// 期望实现 (Phase 2+):
//   - 降级为 Duff's device 状态机：switch(<fn>_coro_state) { case 0: ... case N: ... }
//   - per-function 状态变量：<fn>_coro_state / <fn>_coro_value / <fn>_coro_done
//   - 声明为文件作用域 static 全局 (跨调用保持; dcc C 后端不支持函数内 static 局部)
// 
// ⚠️ Phase 1 说明:
//   - 当前 co_await/co_yield/co_return 未实现 runtime 支持
//   - 测试标记为 compile-only 以验证语法级支持
//   - 运行时执行在 future PR 中实现

void f() {
    co_yield 1;
    co_yield 2;
    co_return;
}

int main() {
    f();
    return 0;
}
