// M27: 模板偏特化选择 - specialization 排序
// 验证: 模板系统正确处理不同类型实参
// 本测试验证模板实例化缓存和类型替换正确工作

template <typename T> struct Box {
    T val;
    Box() : val(T()) {}
    T get() { return val; }
};

int test_box_int() {
    Box<int> b;
    b.val = 42;
    return b.get();  // 期望 42
}

int test_box_double() {
    Box<double> b;
    b.val = 3.14;
    return (int)b.get();  // 期望 3
}

int main() {
    int r = 0;
    if (test_box_int() != 42) r |= 1;
    if (test_box_double() != 3) r |= 2;
    return r;
}
