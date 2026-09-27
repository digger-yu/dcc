// ranges.cpp - std::ranges 简化版头 (M18) 真实调用夹具
//
// 关键回归点: `<ranges>` 头里的 `at()` 直接写 `memcpy(&r, p, sizeof(int))`,
// 而头本身既不 #include <cstring> 也没有 extern "C" 原型 —— 与 libstdc++ 一样
// 依赖编译器的 -fbuiltin 隐式内置声明. native pipeline 必须把 memcpy/memset/
// memmove/strlen 等 libc 内存函数当第一类内置符号注入, 否则这里报
// `undeclared identifier: memcpy`.

#include <ranges>

static int is_even(int x) { return (x % 2) == 0; }
static int triple(int x) { return x * 3; }

// 1) 显式构造 5 个 view 类, 并逐个调用 size()/empty()/at().
//    每个 at() 的函数体里都有一次 memcpy 调用需要被前端接受并降到 libc 外调用.
int use_views_directly(dccpp_view* src) {
    std::views::filter_view f(src, is_even);
    std::views::transform_view t(src, triple);
    std::views::take_view k(src, 2);
    std::views::reverse_view r(src);
    std::views::iota_view i(src);
    int acc = 0;
    acc += (int)f.size();
    acc += (int)t.size();
    acc += (int)k.size();
    acc += (int)r.size();
    acc += (int)i.size();
    acc += f.at(0);
    acc += t.at(1);
    acc += k.at(2);
    acc += r.at(3);
    acc += i.at(0);
    acc += f.empty();
    return acc;
}

// 2) std::ranges 工厂函数 (返回 view 对象) —— 覆盖 inline 工厂 + 按值返回路径.
int use_ranges_factories(dccpp_view* src) {
    std::views::filter_view f = std::ranges::filter(src, is_even);
    std::views::transform_view t = std::ranges::transform(src, triple);
    std::views::take_view k = std::ranges::take(src, 4);
    std::views::reverse_view r = std::ranges::reverse(src);
    int acc = 0;
    acc += f.at(0);
    acc += t.at(0);
    acc += k.at(0);
    acc += r.at(0);
    acc += (int)f.size();
    return acc;
}

// 3) 内存/字符串 builtin 在用户代码里同样必须无需声明即可用.
int use_memory_builtins() {
    int dst[4];
    int src_arr[4];
    src_arr[0] = 1;
    src_arr[1] = 2;
    src_arr[2] = 3;
    src_arr[3] = 4;
    memset(dst, 0, sizeof(dst));
    memcpy(dst, src_arr, sizeof(dst));
    memmove(dst + 1, src_arr, sizeof(int) * 3);
    if (dst[0] != 1) return 1;
    const char* s = "abc";
    if ((int)strlen(s) != 3) return 2;
    char buf[4];
    strncpy(buf, s, 4);
    if (buf[2] != 'c') return 3;
    return 0;
}

// 空 view 上的确定值: size()==0, at(i) 越界返回 0, empty()==1.
int main() {
    dccpp_view* src = dccpp_view_new(sizeof(int));
    if (use_views_directly(src) != 1) return 1;
    if (use_ranges_factories(src) != 0) return 2;
    return use_memory_builtins();
}
