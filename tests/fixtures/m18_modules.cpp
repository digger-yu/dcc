// m18_modules.cpp - C++20 Modules 占位 smoke test
// 验证: `module;` `export module name;` `export int f() {}` `import name;`
// 语法被接受, lower 不报错. 运行时不做真实 module 链接 (M19+).

module;

export module dcc_test;

export int answer() { return 42; }

int main() {
    return answer() - 42;
}
