// tests/fixtures/c/c23_constexpr_var.c
// C23 constexpr 变量: 必须常量初始化, 隐含 const.

// constexpr 变量用常量表达式初始化
constexpr int max_size = 1024;
constexpr int version = 100 + 23;  // 编译期计算
constexpr long big = 1LL << 32;

// constexpr 与 sizeof 结合
constexpr int ptr_size = sizeof(void *);

// 验证 constexpr 变量是 const (只读)
// constexpr float pi = 3.14;  // 浮点常量

int main(void) {
    // constexpr 变量可正常使用
    if (max_size != 1024) return 1;
    if (version != 123) return 2;
    if (ptr_size != 8) return 3;  // LP64: pointer = 8 bytes
    
    // 可用于需要常量的上下文
    switch (max_size) {
        case 1024: break;
        default: return 4;
    }
    
    return 0;
}
