// m26_modules_bmf.cpp - C++20 modules BMF 序列化 smoke test
// 验证: export module / export 函数 / export 类被解析与降级,
// lower 在 export module 声明后收集导出声明并序列化为 BMF 文本.

export module bmf_test;

export int add(int a, int b) { return a + b; }

export class Point {
public:
    int x;
    int y;
};

int main() {
    return add(1, 2) - 3;
}
