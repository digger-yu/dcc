// M23: __attribute__((retain)) + __attribute__((used)) 联合使用
// 测试目标: 编译通过, 静态变量与函数均被保留
__attribute__((retain, used)) static int combo_var = 7;

__attribute__((retain, used)) static int combo_fn(int x) {
    return x + 35;
}

int main(void) {
    return combo_var + combo_fn(0);  // 7 + 35 = 42
}
