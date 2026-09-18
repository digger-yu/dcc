// Coroutine co_await smoke test
// ⚠️ Phase 1: compile-only test (runtime support pending)
// 验证：co_await 语法解析正确性
// 
// 期望实现 (Phase 2+):
//   - Promise object with awaitable support
//   - Suspend/resume semantics via coroutine_frame ABI
//   - co_await → suspend_always + resume logic
// 
// ⚠️ Phase 1 说明:
//   - 当前 co_await 未实现 runtime 支持
//   - 测试标记为 compile-only 以验证语法级支持

int awaitable() {
    co_await 1;
    co_await 2;
    co_return 42;
}

int main() {
    if (awaitable() != 1) return 1;
    if (awaitable() != 2) return 2;
    if (awaitable() != 42) return 3;
    return 0;
}
