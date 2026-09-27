// multi_declarator_typedef.cpp —— typedef 声明里的多个 init-declarator
//
// `typedef` 同样是「decl-specifier + init-declarator 列表」这一条形
// ([dcl.typedef]): `typedef int C, *P;` 一次声明**两个**别名, 且指针层级属于各自
// 的 declarator —— `C` 是 `int`, 只有 `P` 是 `int*`。旧实现只认最后一个名字
// (整条降级成 unsupported opaque declaration), 并且类型名消歧 (`sizeof(T)`) 依赖
// 「`;` 前最后一个 Ident」的启发式, 逗号之前的别名会漏登记。
//
// 验证点 (退出码逐条与 g++ -std=c++20 真值对照):
//   1) 一条 typedef 的多个别名都可用 (`A a; B b;`), 且各自类型正确;
//   2) `typedef int C, *P;` —— sizeof(C)==sizeof(int) 且 sizeof(P)==sizeof(int*);
//   3) 逗号之前的别名也能被 `sizeof(别名)` 认成类型 (不是变量名);
//   4) 数组别名 `typedef unsigned char u8, u8x4[4];` 各自的 sizeof / 下标读写;
//   5) 一条声明里的 4 个别名互不串味 (`typedef long L, *LP;` 的 L 是 long);
//   6) 类内一条 typedef 的多个成员别名都能当声明类型用
//      (`template <class T> struct Box { typedef T Val, Val2; };`)。
typedef int A, B;
typedef int C, *P;
typedef unsigned char u8, u8x4[4];

typedef long L, *LP;

int use_aliases() {
    A a = 1;
    B b = 2;
    return a + b;
}

int probe_pointer_alias() {
    C c = 5;
    P p = &c;
    if (sizeof(C) != sizeof(int)) {
        return 2;
    }
    if (sizeof(P) != sizeof(int*)) {
        return 3;
    }
    if (*p != 5) {
        return 4;
    }
    c = 7;
    if (*p != 7) {
        return 5;
    }
    return 0;
}

int probe_array_alias() {
    u8x4 v;
    u8 s = 0;
    v[0] = 1;
    v[1] = 2;
    v[2] = 3;
    v[3] = 4;
    if (sizeof(u8) != 1) {
        return 6;
    }
    if (sizeof(u8x4) != 4) {
        return 7;
    }
    for (int i = 0; i < 4; i = i + 1) {
        s = s + v[i];
    }
    if (s != 10) {
        return 8;
    }
    // 下标写回仍走同一元素类型, 不越界不串味 (`v[3] = v[0] + v[1] + 4` 得 7)
    v[3] = v[0] + v[1] + 4;
    if (v[3] != 7) {
        return 9;
    }
    u8 head = v[0] + v[1] + v[2];
    if (head != 6) {
        return 13;
    }
    return 0;
}

template <class T>
struct Box {
    // 一条成员 typedef 带两个 declarator: 两个都必须随实例化被代入
    typedef T Val, Val2;
};

int probe_member_typedef() {
    Box<int>::Val v = 10;
    Box<int>::Val2 w = 5;
    v = v + w;
    if (v != 15) {
        return 17;
    }
    w = w + v;
    if (w != 20) {
        return 18;
    }
    return 0;
}

int main() {
    if (use_aliases() != 3) {
        return 1;
    }
    if (probe_pointer_alias() != 0) {
        return 11;
    }
    if (probe_array_alias() != 0) {
        return 12;
    }
    if (probe_member_typedef() != 0) {
        return 13;
    }
    // `L` 与 `LP` 来自同一条 typedef: L 是 long, LP 才是 long*
    L one = 1;
    LP pp = &one;
    if (sizeof(L) != sizeof(long)) {
        return 14;
    }
    if (sizeof(LP) != sizeof(long*)) {
        return 15;
    }
    one = one + 41;
    if (*pp != 42) {
        return 16;
    }
    return 0;
}
