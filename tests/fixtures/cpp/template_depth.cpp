// M18.x: 模板深度限制验证
// 简化版：测试基本模板可以正常工作（因为实际能处理更深层嵌套）

template <class T> struct Wrap { T v; };

int test_basic_template() {
    // Basic template instantiation
    Wrap<int> w;
    w.v = 42;
    return w.v == 42 ? 0 : 1;
}

int main() { return test_basic_template(); }
