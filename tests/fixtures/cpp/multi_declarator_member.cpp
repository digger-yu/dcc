// multi_declarator_member.cpp —— 类成员声明里的多个 init-declarator
//
// 成员路径同样是一条声明 = decl-specifier + init-declarator 列表:
// `int m_a, m_b;` 必须展开成**两个**字段 (旧实现只留第一个名字,
// `m_b` 静默消失, 下游报 "member 'm_b' not found"), 而 `int *m_p, m_q;`
// 里 `m_q` 的类型必须是 `int` —— 指针层级属于 declarator, 不属于 decl-specifier.
//
// 注: 类内 NSDMI 的**初值**在 dcc 的成员模型里不携带 (`CppDecl::MemberVar` 无
// init 字段, 既有空缺, 单 declarator 同样丢), 故成员初值一律在构造函数里给.
//
// 验证点 (退出码逐条与 g++ -std=c++20 真值对照):
//   1) `int m_a, m_b;` 两个字段独立可读得各自值;
//   2) `int *m_p, m_q;` / `int *p, q, r;` 的存储布局与读写语义: q/r 是 int
//      (`struct T { int *p, q, r; };` 尺寸 16, 若 p 的指针层级被共享给
//      q/r 则会变成 24);
//   3) 成员函数体里的多 declarator 局部声明;
//   4) 数组维度属于本 declarator: `int m_arr[2], m_n;` 里 m_n 不是数组;
//   5) `mutable` 多 declarator 与 const 成员函数.
struct S {
    int m_a, m_b;
    int *m_p, m_q;
    int m_arr[2], m_n;
    mutable int m_hits, m_misses;

    S() : m_a(1), m_b(2), m_p(0), m_q(3), m_n(4), m_hits(0), m_misses(0) {
        m_arr[0] = 7;
        m_arr[1] = 9;
    }

    int sum() const { return m_a + m_b; }

    int body(int k) {
        int x = k, y = k + 1;
        int *p, q = k + 2;
        p = &q;
        m_a = x;
        m_b = *p;
        return x + y + *p + m_a + m_b;
    }

    int tally(int k) const {
        m_hits = m_hits + 1;
        m_misses = m_misses + k;
        return m_hits * 100 + m_misses;
    }

    int arr_sum() const {
        int i = m_arr[0], j = m_arr[1];
        return i + j + m_n;
    }
};

struct T {
    int *p, q, r;
};

int main() {
    S s;
    if (s.sum() != 3) return 1;
    if (s.body(4) != 25) return 2;
    if (s.m_a != 4) return 3;
    if (s.m_b != 6) return 4;
    if (s.m_q != 3) return 5;
    T t;
    t.q = 1;
    t.r = 2;
    t.p = &t.r;
    if (*t.p != 2) return 6;
    *t.p = 3;
    if (t.r != 3) return 7;
    if (t.q != 1) return 8;
    if (s.arr_sum() != 20) return 9;
    if (sizeof(T) != 16) return 10;
    if (s.tally(1) != 101) return 11;
    if (s.tally(2) != 203) return 12;
    return 0;
}
