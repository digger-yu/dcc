/* examples/builtins.c - 编译期内建函数与扩展测试 */

/* __attribute__((packed)): 无填充结构体 */
struct packed_s {
    char a;
    int  b;
    char c;
} __attribute__((packed));

/* __attribute__((aligned)): 对齐结构体 */
struct aligned_s {
    char a;
    int  b;
} __attribute__((aligned(16)));

/* __attribute__((unused)): 变量属性 */
int unused_var __attribute__((unused)) = 0;

int main() {
    /* sizeof(expr): 应返回实际类型大小 */
    int x = 0;
    int sz_int = sizeof(x);        /* 应为 4 */
    int sz_char = sizeof(char);    /* 应为 1 */

    /* _Alignof: 对齐查询 */
    int al_int = _Alignof(int);    /* 应为 4 */
    int al_char = _Alignof(char);  /* 应为 1 */

    /* __builtin_expect: 分支预测提示 (透传 expr) */
    int cond = 1;
    int val = __builtin_expect(cond, 1);

    /* __builtin_constant_p: 编译期常量判断 (返回 0) */
    int is_const = __builtin_constant_p(42);

    /* typeof: 类型推导 */
    typeof(x) y = 10;

    /* __extension__: 抑制警告 (直接跳过) */
    __extension__ int z = 20;

    /* 语句表达式 ({...}) */
    int r = ({ int a = 3; int b = 4; a + b; });

    /* packed 结构体大小: 1+4+1 = 6 */
    int packed_sz = sizeof(struct packed_s);

    /* 验证: r == 7 时返回 0 (成功) */
    if (r == 7) return 0;
    return 1;
}
