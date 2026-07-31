// M23: __builtin_debugtrap / __builtin_assume_aligned / __builtin_alloca
// 测试目标: 编译通过, 程序可执行
// 注意: __builtin_debugtrap 实际触发 int3, 这里把它放在不会执行的分支
//       __builtin_alloca 分配栈空间并写入读出 (使用简单指针操作, 避免 for 循环)
int main(void) {
    if (0) {
        __builtin_debugtrap();
    }
    // 16 字节对齐假设: 编译器应原样返回参数
    int *p = (int *)__builtin_assume_aligned((void *)0x10000, 16);
    // 32 字节栈分配, 写入 4 个 int
    int *m = (int *)__builtin_alloca(32);
    m[0] = 10;
    m[1] = 11;
    m[2] = 12;
    m[3] = 9;  // 让 sum 为 42
    int sum = m[0] + m[1] + m[2] + m[3];
    (void)p;
    return sum;  // 期望 42
}
