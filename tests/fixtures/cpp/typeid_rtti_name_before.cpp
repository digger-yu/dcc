// Stage 7 任务 2: type_info 对象的 name() / before() 运行期语义
//
// dcc 发射的 typeinfo 对象按 Itanium ABI 叠层 (偏移 0 = type_info 虚表槽,
// 偏移 8 = `__name` 指向 mangled 类型名字符串), 因此 `name()` 读到的必须
// 与 g++ 在同一 `_ZTI<T>` 对象上读到的**同一个字符串**。这里用逐位退出码
// 表达断言: 任一断言不符 -> 对应 bit 置位, 全对 -> 0。
//
// 覆盖面:
//   bit1  简单类的 name() == "3Dog" (mangled 类型名, 不是 "_ZTI...")
//   bit2  命名空间类的 name() == "N2ns6WidgetE" (嵌套名 mangle 形式)
//   bit4  多态表达式 typeid(*p) 动态取到最派生类的 name()
//   bit8  静态 typeid(T) 与动态 typeid(*p) 指向同一 typeinfo 对象
//   bit16 before() 按 name() 字典序成立 (单继承类 __si_class_type_info)
//   bit32 before() 反向 / 自反为 false
//   bit64 operator!= 与 operator== 互补
//
// 注意: 断言里刻意不使用「文件作用域带初始化式的量」(该情形 dcc 目前会
// 静默丢掉初始化式, 属另一处缺口), 也不使用 `f()[0]` 形式。
#include <typeinfo>

struct Dog {
    virtual ~Dog() {}
};

struct Base {
    virtual ~Base() {}
    virtual void f() {}
};

struct Mid : Base {
    void f() override {}
};

namespace ns {
struct Widget {
    virtual ~Widget() {}
};
} // namespace ns

// 字符串比较自带一份, 避免依赖 C 运行库头之间的循环依赖。
static int eqstr(const char* a, const char* b) {
    int i = 0;
    while (1) {
        if (a[i] != b[i]) return 0;
        if (a[i] == 0) return 1;
        i = i + 1;
    }
}

int main() {
    int r = 0;

    // bit1: name() 返回 mangled 类型名
    const std::type_info& tdog = typeid(Dog);
    if (!eqstr(tdog.name(), "3Dog")) r = r | 1;

    // bit2: 命名空间限定类的 mangled 名 (`_ZTSN2ns6WidgetE` 的内容)
    const std::type_info& tw = typeid(ns::Widget);
    if (!eqstr(tw.name(), "N2ns6WidgetE")) r = r | 2;

    // bit4: 动态路径 typeid(*p) 取最派生类
    Mid m;
    Base* p = &m;
    const std::type_info& dyn = typeid(*p);
    if (!eqstr(dyn.name(), "3Mid")) r = r | 4;

    // bit8: 静态与动态对同一对象给出同一个 typeinfo
    const std::type_info& stat = typeid(Mid);
    if (!(stat == dyn)) r = r | 8;

    // bit16: "3Dog" < "4Base" (按 name() 的 strcmp 序)
    {
        const std::type_info& tb = typeid(Base);
        const std::type_info& t1 = typeid(Dog);
        if (!t1.before(tb)) r = r | 16;

        // bit32: 反向不成立, 且自反不成立
        if (tb.before(t1)) r = r | 32;
        if (tb.before(tb)) r = r | 32;
    }

    // bit64: operator!= 与 operator== 互补
    {
        const std::type_info& tw2 = typeid(ns::Widget);
        if (!(tdog != tw2)) r = r | 64;
        if (tdog == tw2) r = r | 64;
        if (!(tw == tw2)) r = r | 64;
    }

    // 后置形式: typeid(T) 的结果是 glvalue, 可直接 .name() / .before()
    if (!eqstr(typeid(Dog).name(), "3Dog")) r = r | 1;
    if (!typeid(Dog).before(typeid(Base))) r = r | 16;

    return r;
}
