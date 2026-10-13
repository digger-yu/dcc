/* ssa_volatile_semantics.c
 * SSA IR 层 volatile 感知: volatile 局部/指针/全局访问
 * 在 -O2 优化下不被 DCE/DSE 消除或重排, 程序语义保持不变.
 */
static volatile int gflag;

/* volatile 局部变量: 两次"看似无用"的写入都必须保留 */
int test_volatile_local(void) {
    volatile int x = 1;
    x = 2;
    x = 3;
    return x;
}

/* volatile 指针: 通过指针的读写都必须保留 */
int test_volatile_pointer(void) {
    volatile int v = 7;
    volatile int *p = &v;
    *p = 11;
    return *p;
}

/* volatile 全局变量: 两次写入与两次读取都必须保留 */
int test_volatile_global(void) {
    gflag = 5;
    gflag = 9;
    int a = gflag;
    int b = gflag;
    return a + b;
}

/* volatile 数组元素访问 */
int test_volatile_array(void) {
    volatile int a[2];
    a[0] = 1;
    a[1] = 4;
    return a[0] + a[1];
}

int main(void) {
    int ok = 1;
    ok &= (test_volatile_local() == 3);
    ok &= (test_volatile_pointer() == 11);
    ok &= (test_volatile_global() == 18);
    ok &= (test_volatile_array() == 5);
    return ok ? 0 : 1;
}
