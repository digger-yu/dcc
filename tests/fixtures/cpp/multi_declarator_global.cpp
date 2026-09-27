// multi_declarator_global.cpp —— 全局声明里的多个 init-declarator ([dcl.dcl]/[basic.start.static])
//
// 一条全局声明 `int a, b;` 在 AST 里按 declarator 展开成 N 个 `CppDecl::GlobalVar`
// (顶层声明容器本就是 Vec<CppDecl>); 旧实现直接抛 "unsupported opaque declaration"
// 把整条声明降级成原文, 两个名字一起丢失.
//
// 注: **全局/命名空间作用域变量的静态初值在 native 后端不发射** (对象落在 .bss,
// 单 declarator 的 `int g = 1;` 同样丢, 见任务报告"仍存在缺陷"一节), 这是与本夹具
// 无关的既有空缺. 故这里对可写对象先赋值再读回, 校验的是「N 个 declarator 是否
// 展开成 N 个独立、类型正确的对象」; const/constexpr 全局只校验对象各自独立存在.
//
// 验证点 (退出码逐条与 g++ -std=c++20 真值对照):
//   1) `int ga, gb;` 是两个**不同对象** (地址不同, 写一个不影响另一个);
//   2) `const int ca = 10, cb = 20;` / `constexpr int xa = 5, xb = 6;` 各自独立;
//   3) `static int sa, sb;` 内部链接的两个对象各自独立;
//   4) **指针层级只属于带 `*` 的那个 declarator**: `int *gp, gv;` 里
//      gv 是 int (sizeof == 4), gp 是 int* (sizeof == 8);
//   5) `volatile` 多 declarator 可独立读写.
volatile int va, vb;
int ga, gb;
const int ca = 10, cb = 20;
constexpr int xa = 5, xb = 6;
static int sa, sb;
int *gp, gv;

int probe() {
    ga = 1;
    gb = 2;
    if (ga + gb != 3) return 1;
    if (&ga == &gb) return 2;
    if (&ca == &cb) return 3;
    if (&xa == &xb) return 4;
    sa = 3;
    sb = 4;
    if (sa + sb != 7) return 5;
    if (&sa == &sb) return 6;
    gv = 5;
    gp = &gv;
    if (*gp != 5) return 7;
    if (sizeof(gv) != sizeof(int)) return 8;
    if (sizeof(gp) != sizeof(int *)) return 9;
    ga = ga + gb;
    gb = gb * 10;
    if (ga != 3) return 10;
    if (gb != 20) return 11;
    va = 1;
    vb = 2;
    va = va + 1;
    vb = vb + 10;
    if (va + vb != 14) return 12;
    if (&va == &vb) return 13;
    return 0;
}

int main() { return probe(); }
