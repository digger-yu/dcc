// incomplete_tag_pointers.cpp — C++ native 后端「不完整类类型」正向夹具 (缺陷 B4)
//
// 覆盖 GCC/glibc 头里大量出现、此前被 native 后端当作 `unknown class type`
// 硬拒的形态: 一个 tag 从未被声明, 只以「指向它的指针 / 指针数组 / typedef 到
// 不完整指针」出现。这些在 C++ 里完全合法 ([basic.types] 允许指向不完整类型的
// 指针; [dcl.type.elab] 允许 elaborated-type-specifier 首次引入 tag)。
//
// 同时固定住「同一个 ClassId 复用」这条关键路径: 指针声明先于完整定义出现时,
// 后续通过该指针的成员访问必须落在同一个类符号上 (否则报 member not found)。
//
// 本夹具只做 g++ 也接受的正向形态; 「对不完整类型取 sizeof / 成员访问」这类
// 必须报错的负向形态由 sema 单元测试与 errors snapshot 覆盖, 不放这里
// (cpp_abi_fixtures_are_standard_cxx 要求本目录每条夹具都能被 g++ 编译)。
#include <cstdio>

// 1) 形似 glibc `struct __locale_struct`: 成员是指向「从未声明」tag 的数组。
struct locale_like {
    struct never_declared *__locales[13];
    const unsigned short *__ctype_b;
    int currencynumber_;
};

// 2) 先用后定义: 指针先出现, 完整定义在后, 使用点必须落在同一个类。
struct late_bound *make_late();
struct late_bound {
    int v;
};

// 3) typedef 到不完整指针 (opaque handle 惯用法)。
typedef struct opaque_tag *opaque_handle;

// 4) 类字段: 指向从未声明 tag 的普通指针。
struct ptr_field {
    struct only_ptr *p;
};

// 5) 形参 / 返回值为不完整类型指针。
int consume_ptr(struct never_declared *l);
struct only_ptr *produce_ptr();

int main() {
    struct locale_like loc;
    loc.currencynumber_ = 42;
    loc.__ctype_b = 0;
    printf("currency=%d\n", loc.currencynumber_);

    struct late_bound lb;
    lb.v = 7;
    struct late_bound *lp = &lb;
    printf("late=%d\n", lp->v); // 验证 ClassId 复用: 成员可访问

    opaque_handle h = 0;
    printf("handle_null=%d\n", (h == 0));

    struct ptr_field q;
    q.p = 0;
    printf("ptr_null=%d\n", (q.p == 0));

    // 指针的 sizeof 与所指类型是否完整无关, 恒等于指针宽度。
    printf("ptrsizeof_nonzero=%d\n", ((int)sizeof(struct never_declared *)) > 0);
    return 0;
}
