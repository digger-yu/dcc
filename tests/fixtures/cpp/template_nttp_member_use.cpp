// template_nttp_member_use.cpp —— 非类型模板形参在**成员体**里的替换 ([temp.names]/[temp.non_type])
//
// 验证点 (退出码逐条与 g++ -std=c++20 真值对照):
//   1) 成员函数体里 `return N;` 要替换成实参值 (旧行为: "undeclared identifier: N",
//      整条成员函数被静默丢弃 -> 链接期 undefined reference);
//   2) 体内 `N` 参与算术 / 比较时同样替换;
//   3) 构造函数成员初始化列表里的 `N`;
//   4) 非类型实参本身是常量表达式 `S<(2 + 5)>` ([temp.arg.nontype]);
//   5) 同一模板不同非类型实参的实例互不串味 (S<7> 与 S<3>), 且各自状态独立.
template <int N>
struct S {
    int v;
    S() : v(N) {}
    int fixed() const { return N; }
    int shifted(int k) const { return N + k; }
    bool is_big() const { return N > 4; }
    int stored() const { return v; }
};

template <int M>
struct Counter {
    int total;
    Counter() : total(M) {}
    int bump() {
        total = total + M;
        return total;
    }
};

int main() {
    S<7> a;
    if (a.fixed() != 7) return 1;
    if (a.stored() != 7) return 2;
    if (a.shifted(3) != 10) return 3;
    if (!a.is_big()) return 4;
    S<3> b;
    if (b.fixed() != 3) return 5;
    if (b.is_big()) return 6;
    if (b.stored() != 3) return 7;
    S<(2 + 5)> c;
    if (c.fixed() != 7) return 8;
    if (c.stored() != 7) return 9;
    Counter<4> d;
    if (d.bump() != 8) return 10;
    if (d.bump() != 12) return 11;
    // 第二个实例的累加不得影响第一个
    Counter<10> e;
    if (e.bump() != 20) return 12;
    if (d.bump() != 16) return 13;
    return 0;
}
