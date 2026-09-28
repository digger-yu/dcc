// Stage 8 交叉验证: 模板**实参位置**的依赖非常量子式 + SFINAE 候选剔除。
//
// 这里钉住三条历史上不成立的口径:
// 1. [temp.arg.nontype]: `enable_if<is_pointer<T>::value, int>::type` 的首实参是
//    一个依赖常量表达式。它必须先在实参表里折成 `NonType(1)`, 否则
//    `enable_if<true, T>` 这个偏特化永远匹配不上 (比较的是 key), 实例名还会把
//    整个子式当类名编码进去 (`..._tm_is_pointer_p_int_value_int`)。
// 2. [temp.dep.constexpr]: `!is_pointer<T>::value` 这种**取反子式**过去在解析期
//    求不出值就直接报错, 顶层容错把整条模板声明降级成 Opaque —— 模板因此从
//    AST 上消失。现在按结构保留 (`MangleType::NtExpr`), 单态化代入实参后再折。
// 3. [temp.deduct]/8: 签名里的依赖名代入后不存在 (`enable_if<false, int>` 里没有
//    `type`) 是**替换失败**, 该候选静默移出候选集, 而不是变成下游的
//    `unknown class type` 硬错误。
#include <cstdio>

struct true_type {
  enum { value = 1 };
};
struct false_type {
  enum { value = 0 };
};

template <typename T>
struct is_pointer : false_type {};
template <typename T>
struct is_pointer<T*> : true_type {};

template <typename T, typename U>
struct is_same : false_type {};
template <typename T>
struct is_same<T, T> : true_type {};

template <bool B, typename T = void>
struct enable_if {};
template <typename T>
struct enable_if<true, T> {
  typedef T type;
};

// (1) 正向子式 + (2) 取反子式: 同一对重载, 靠 SFINAE 二选一。
template <typename T>
typename enable_if<is_pointer<T>::value, int>::type pick_ptr(T) {
  return 1;
}
template <typename T>
typename enable_if<!is_pointer<T>::value, int>::type pick_ptr(T) {
  return 2;
}

// (2) `||` / `&&` 组合的依赖子式同样是合法的非类型实参。
template <typename T>
typename enable_if<is_same<T, int>::value || is_same<T, long>::value, int>::type
rank(T) {
  return 7;
}
template <typename T>
typename enable_if<is_same<T, int>::value && !is_pointer<T>::value, int>::type
rank(T, int) {
  return 9;
}

// traits 常量本身也要能被当**值**用 (折叠后的 NonType 进入实例名)。
template <typename T>
struct probe {
  int value() const { return is_pointer<T>::value + (!is_pointer<T>::value) * 10; }
};

int main() {
  int i = 7;
  int* p = &i;
  printf("ptr=%d nonptr=%d\n", pick_ptr(p), pick_ptr(i));
  printf("rank_int=%d rank_int2=%d\n", rank(i), rank(i, 0));
  probe<int*> a;
  probe<int> b;
  printf("probe=%d,%d\n", a.value(), b.value());
  return 0;
}
