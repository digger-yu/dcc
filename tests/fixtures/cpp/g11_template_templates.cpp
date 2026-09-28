// G11: Template template parameter support
// Validates parser can recognize and handle `template<typename...> class Name` syntax
//
// 挂载说明 (§7.4-B): 本夹具原先 `#include <vector>` + `#include <deque>`, 而 dcc 的
// std 头注册表 (src/cpp/stdlib/mod.rs 的 STD_HEADERS) 里没有 `deque` —— `#include <deque>`
// 会以 E0206 "include file not found" 先失败, 把 G11 真正要验的东西 (模板模板形参) 全部
// 挡住, 使夹具既不是正例也无从判定. 这里改用 `std::vector` 一个 std 容器 + 一个用户自
// 定义的类模板 `Box<T>`, "两个不同的容器实参" 这一覆盖点保持不变, 且仍是标准 C++20.

#include <vector>

// 用户自己的容器类模板: 作为模板模板实参与 std::vector 形成两个不同实参
template<typename T>
class Box {
public:
    T v;
};

// Primary template with template template parameter
template<typename T, template<typename> class Container>
class Adapter {
public:
    Container<T> data;
    
    void push(T value) {
        // Simplified - no runtime needed for parsing test
    }
};

// Specialization with different container type
template<template<typename> class Container>
class Adapter<void, Container> {
public:
    Container<void*> data;
};

// Test basic instantiation
int main() {
    // These should parse without errors
    Adapter<int, std::vector> v1;
    Adapter<double, Box> v2;
    
    (void)v1;
    (void)v2;
    
    return 0;
}
