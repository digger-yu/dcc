// M27: C++20 Modules ODR 检查
// 验证: export module / import 语法被接受, ODR 检查器检测跨模块重复定义
// 本测试验证模块语法被正确解析, ODR 检查在加载多个模块时执行

module;

export module odr_test;

export int add(int a, int b) { return a + b; }

export class Calculator {
public:
    int value;
    Calculator() : value(0) {}
    int compute(int x) { return x * 2; }
};

int main() {
    Calculator c;
    c.value = add(3, 4);
    return c.value - 7;  // 期望 0
}
