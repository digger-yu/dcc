// 测试重要级特性: __builtin_offsetof, __has_builtin, MODULE_LICENSE, percpu

// 测试 __builtin_offsetof
struct test_struct {
    int a;
    long b;
    char c;
    int d;
};

struct nested_struct {
    char x;
    struct test_struct inner;
    int y;
};

int test_offsetof() {
    int off_a = __builtin_offsetof(struct test_struct, a);  // 0
    int off_b = __builtin_offsetof(struct test_struct, b);  // 8 (after int + padding)
    int off_c = __builtin_offsetof(struct test_struct, c);  // 16
    int off_d = __builtin_offsetof(struct test_struct, d);  // 20
    int off_inner = __builtin_offsetof(struct nested_struct, inner);  // 8 (after char + padding)
    int off_y = __builtin_offsetof(struct nested_struct, y);

    if (off_a != 0) return 1;
    if (off_b != 8) return 2;
    if (off_c != 16) return 3;
    if (off_d != 20) return 4;
    if (off_inner != 8) return 5;
    return 0;
}

// 测试 MODULE_LICENSE 宏
MODULE_LICENSE("GPL");
MODULE_AUTHOR("Test Author");
MODULE_DESCRIPTION("Test module");

// 测试 percpu 宏
DEFINE_PER_CPU(int, per_cpu_var);

int main() {
    int ret = test_offsetof();
    if (ret != 0) return ret;
    return 0;
}
