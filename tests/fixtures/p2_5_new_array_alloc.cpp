// P2.5: new T[n] 数组分配 (Section 6).
// 验证 dcc parser 检测 `new T[n]` 语法, lower 阶段 emit
// `((T*)mangle_op_new_array()(sizeof(T) * n))` 形式, 即
// Itanium C++ ABI 数组分配符号 _Znam 调用. 同时覆盖:
//   1. 类类型 T (非内建) 走 (struct T*)
//   2. 计数为变量表达式 (非字面量)
//   3. delete[] 走 _ZdaPv (同 P2.4)
struct Cell {
    int v;
};
int test_new_typed_array(int n) {
    // 类类型 + 变量计数: 走 _Znam(sizeof(struct Cell) * n)
    Cell* cs = new Cell[n];
    cs[0].v = 7;
    cs[1].v = 11;
    int sum = 0;
    if (n >= 1) sum = sum + cs[0].v;
    if (n >= 2) sum = sum + cs[1].v;
    delete[] cs;  // 走 _ZdaPv
    return sum;   // n=2 时返回 18
}
int test_new_array_entry() {
    return test_new_typed_array(2);
}
