// m26_coroutine_runtime.cpp - 协程真状态机运行时验证
// 验证: co_yield/co_return 降级为 Duff's device 状态机, 且状态跨调用保持.
// 状态/返回槽变量为文件作用域 static 全局 (dcc C 后端不支持函数内
// static 局部, 故不能使用函数内 static).

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
