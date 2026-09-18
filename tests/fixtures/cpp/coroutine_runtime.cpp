// m26_coroutine_runtime.cpp - 协程状态机运行时验证
// ⚠️ Phase 1: compile-only test (runtime support pending)
// 验证：co_yield/co_return 降级为 Duff's device 状态机，且状态跨调用保持
// 
// 期望实现 (Phase 2+):
//   - Duff's device switch-case lowering
//   - File-scope static state variables (gen_coro_state, gen_coro_value, gen_coro_done)
//   - State preservation across calls
// 
// ⚠️ Phase 1 说明:
//   - 当前 co_yield/co_return 未实现 runtime 支持
//   - 测试标记为 compile-only 以验证语法级支持

int gen() {
    co_yield 1;
    co_yield 2;
    co_yield 3;
    co_return 0;
}

int main() {
    if (gen() != 1) return 1;
    if (gen() != 2) return 2;
    if (gen() != 3) return 3;
    if (gen() != 0) return 4;
    // 终态后继续调用仍返回 0
    if (gen() != 0) return 5;
    return 0;
}
