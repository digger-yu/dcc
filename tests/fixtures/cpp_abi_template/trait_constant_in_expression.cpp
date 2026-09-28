// Stage 8 交叉验证: 类型特征的**编译期常量**在表达式里被折叠。
//
// `is_pointer<int*>::value` 这类写法有两处历史上不成立:
// 1. sema 的 `TplStaticAccess` 分支丢掉实参表, 把 `value` 当普通非限定名查
//    (报 `undeclared tpl static access: value`);
// 2. 模板单态化把类名换成实例名后, `Member(Ident(类名), "value")` 被照普通对象
//    成员访问处理 (报 `identifier ... is not a value`)。
// traits 的 `value` 又几乎都是**继承**来的 (`is_pointer<T*> : true_type`), 因此
// 现在按实例化后的类符号沿基类链取常量 ([class.member.lookup])。
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

// 实例体内引用特征常量: 单态化后是「限定名 + 成员」的形态。
template <typename T>
struct ptr_rank {
  int value_of() const { return is_pointer<T>::value; }
};

int probe() {
  int p = is_pointer<int*>::value;
  int np = is_pointer<int>::value;
  int same = is_same<long, long>::value;
  int diff = is_same<long, int>::value;
  printf("p=%d np=%d same=%d diff=%d\n", p, np, same, diff);
  ptr_rank<int*> a;
  ptr_rank<int> b;
  printf("rank=%d,%d\n", a.value_of(), b.value_of());
  return p + np * 10 + same * 100 + diff * 1000;
}

int main() {
  printf("sum=%d\n", probe());
  return 0;
}
