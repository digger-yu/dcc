// M23: constructor(N) / destructor(N) 带优先级
// GCC 约定: 数字越小优先级越高
// 100 在 300 之前执行 (即 init_100 先, init_300 后)
// 析构: 100 在 300 之后 (LIFO) — 但本测试只断言构造期累计值
static int order_counter = 0;

__attribute__((constructor(300))) static void init_300(void) {
    order_counter = order_counter * 10 + 1;
}

__attribute__((constructor(100))) static void init_100(void) {
    order_counter = order_counter * 10 + 2;
}

int get_init_value(void) { return order_counter; }

int main(void) {
    // init_100 (prio=100, 优先) 先跑 -> 0*10+2 = 2
    // init_300 (prio=300) 后跑       -> 2*10+1 = 21
    return get_init_value();  // 期望 21
}
