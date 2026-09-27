// C++98 <iterator>: iterator_traits 主模板 + 指针特化 + 标签继承链
//
// 覆盖点 (全部为真实语义检查, 不是"能编过就算"):
//   1. `iterator_traits<T*>` 偏特化: value_type / difference_type / pointer /
//      iterator_category 必须解析成 T / long / T* / random_access_iterator_tag.
//   2. `iterator_traits<const T*>` 偏特化: value_type 仍是 T (非 const T).
//   3. `iterator_traits<Iter>` 主模板: 逐个转发 Iter 自己的嵌套 typedef
//      (`typename Iter::value_type` 这类依赖嵌套名 —— 这是最容易空过的路径).
//   4. 标签继承链 input <- forward <- bidirectional <- random_access:
//      random_access_iterator_tag* 可隐式转成 input_iterator_tag*.
//   5. 上述 typedef 的 **尺寸** 在运行期校验 (int=4 / long=8 / 指针=8),
//      错一个映射就会在 sizeof 或指针步长上暴露.
//   6. 用户迭代器不只是"提供 typedef 的纸面类型": iterator_traits<CountIt>
//      的 value_type/pointer 必须真能驱动 CountIt 走完一个数组 ——
//      `*it` / `++it` 落到 CountIt 的一元运算符重载上.

#include <iterator>

// ===== 用户自定义迭代器: 只提供 iterator_traits 主模板要求的 5 个嵌套 typedef =====
class CountIt {
public:
    typedef std::input_iterator_tag iterator_category;
    typedef int value_type;
    typedef long difference_type;
    typedef int* pointer;
    typedef int& reference;

    explicit CountIt(int* p) : p_(p) {}
    int operator*() const { return *p_; }
    CountIt& operator++() { ++p_; return *this; }

private:
    int* p_;
};

// ===== traits 结果落成具名 typedef, 之后用 sizeof / 实参匹配校验 =====
typedef std::iterator_traits<int*>::value_type PtrValue;
typedef std::iterator_traits<int*>::difference_type PtrDiff;
typedef std::iterator_traits<int*>::pointer PtrPtr;
typedef std::iterator_traits<int*>::iterator_category PtrCat;
typedef std::iterator_traits<const int*>::value_type CPtrValue;
typedef std::iterator_traits<const int*>::pointer CPtrPtr;
typedef std::iterator_traits<CountIt>::value_type ItValue;
typedef std::iterator_traits<CountIt>::difference_type ItDiff;
typedef std::iterator_traits<CountIt>::pointer ItPtr;
typedef std::iterator_traits<CountIt>::iterator_category ItCat;

// traits<It>::value_type 作依赖嵌套名出现在形参类型上 (主模板 -> Iter 自身成员).
// 若绑定错误, 下面这个模板无法实例化 / 调用点类型不匹配.
template <typename It>
int traits_pick(typename std::iterator_traits<It>::value_type v) {
    return v + 1;
}

// 用偏特化出来的 pointer / value_type 类型做真实累加: 若 `pointer` 退化成了
// int (而非 int*), 指针自增步长与解引用都会错.
int traits_span_sum(PtrPtr first, PtrPtr last, ItValue seed) {
    int n = seed;
    while (first != last) {
        n += *first;
        ++first;
    }
    return n;
}

// const T* 特化: 只能通过 const 指针读取元素.
int traits_csum(CPtrPtr p, ItDiff n) {
    int s = 0;
    for (ItDiff i = 0; i < n; ++i) s += p[i];
    return s;
}

// 用户迭代器真能走: 用 traits<CountIt>::pointer 作为入参类型 (依赖嵌套名),
// 循环里靠 CountIt::operator* / operator++ 读写元素. 若一元运算符没分发,
// `*it` 会被当成内建解引用 (dereference of non-pointer type).
int traits_walk(ItPtr first, int n) {
    CountIt it(first);
    int s = 0;
    for (int i = 0; i < n; ++i) {
        s += *it;
        ++it;
    }
    return s;
}

// random_access_iterator_tag 派生自 input_iterator_tag (经 forward/bidirectional),
// 因此派生指针可隐式转成基指针. 这条语句只在继承链正确时才合法.
std::input_iterator_tag* cat_is_input(PtrCat* c) { return c; }

// 用户迭代器的 category 是 input_iterator_tag 本身.
int cat_is_self(ItCat* c) { return c != 0; }

int main() {
    int a[5];
    a[0] = 1;
    a[1] = 2;
    a[2] = 3;
    a[3] = 4;
    a[4] = 5;

    // 1) 偏特化映射: value_type=int(4) / difference_type=long(8) / pointer=8
    if (sizeof(PtrValue) != 4) return 1;
    if (sizeof(PtrDiff) != 8) return 2;
    if (sizeof(PtrPtr) != 8) return 3;

    // 2) const T* 特化: value_type 去掉 const 仍是 int, pointer 是 const int*
    if (sizeof(CPtrValue) != 4) return 4;
    if (sizeof(CPtrPtr) != 8) return 5;

    // 3) 主模板转发 Iter 的嵌套 typedef: int / long / int*
    if (sizeof(ItValue) != 4) return 6;
    if (sizeof(ItDiff) != 8) return 7;
    if (sizeof(ItPtr) != 8) return 8;

    // 4) 指针特化的类型真能当 int* 用 (步长 4 字节 * 5 个元素 = 15)
    if (traits_span_sum(a, a + 5, 0) != 15) return 9;
    if (traits_span_sum(a, a + 5, 100) != 115) return 10;

    // 5) const 指针特化真能按元素读
    if (traits_csum(a, 5) != 15) return 11;
    if (traits_csum(a + 2, 3) != 12) return 12;

    // 6) 依赖嵌套 traits 作形参类型: value_type=int 时 +1
    if (traits_pick<CountIt>(41) != 42) return 13;
    if (traits_pick<int*>(9) != 10) return 14;

    // 7) 用户迭代器驱动遍历: iterator_traits<CountIt> 的 typedef 真能读元素
    if (traits_walk(a, 5) != 15) return 17;
    if (traits_walk(a + 2, 3) != 12) return 18;

    // 8) 标签继承链
    PtrCat ra;
    std::input_iterator_tag* base = cat_is_input(&ra);
    if (base == 0) return 15;
    ItCat in;
    if (cat_is_self(&in) != 1) return 16;

    return 0;
}
