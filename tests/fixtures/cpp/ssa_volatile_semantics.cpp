// ssa_volatile_semantics.cpp
// C++ native path (lower_ir.rs) SSA IR 层 volatile 感知:
// volatile 局部/指针/全局/结构体成员/箭头访问
// 在 -O2 优化下不被 DCE/DSE 消除或重排, 程序语义保持不变.

static volatile int gflag;

/* volatile 局部变量: 两次"看似无用"的写入都必须保留 */
int test_volatile_local() {
    volatile int x = 1;
    x = 2;
    x = 3;
    return x;
}

/* volatile 指针: 通过指针的读写都必须保留 */
int test_volatile_pointer() {
    volatile int v = 7;
    volatile int *p = &v;
    *p = 11;
    return *p;
}

/* volatile 全局变量: 两次写入与两次读取都必须保留 */
int test_volatile_global() {
    gflag = 5;
    gflag = 9;
    int a = gflag;
    int b = gflag;
    return a + b;
}

/* volatile 数组元素访问: 每次读写都必须落到内存 */
int test_volatile_array() {
    volatile int a[2];
    a[0] = 1;
    a[1] = 4;
    a[0] = 2;
    return a[0] + a[1];
}

/* volatile 结构体成员（字段是 volatile） */
struct Sensor {
    volatile int reg;
    int normal;
};

int test_volatile_struct_member() {
    Sensor s;
    s.reg = 0xAA;
    s.reg = 0xBB;
    int val = s.reg;
    s.normal = 42;
    return val + s.normal;
}

/* volatile 对象的成员访问（整个对象是 volatile） */
struct Device {
    int ctrl;
    int data;
};

int test_volatile_object_member() {
    volatile Device dev;
    dev.ctrl = 1;
    dev.data = 2;
    int c = dev.ctrl;
    int d = dev.data;
    return c + d;
}

/* volatile 自增自减 */
int test_volatile_incdec() {
    volatile int x = 0;
    x++;
    ++x;
    x--;
    --x;
    return x;
}

int main() {
    int ok = 1;
    ok &= (test_volatile_local() == 3);
    ok &= (test_volatile_pointer() == 11);
    ok &= (test_volatile_global() == 18);
    ok &= (test_volatile_array() == 6);
    ok &= (test_volatile_struct_member() == 0xBB + 42);
    ok &= (test_volatile_object_member() == 3);
    ok &= (test_volatile_incdec() == 0);
    return ok ? 0 : 1;
}
