// Stage 7 交叉验证: RTTI / dynamic_cast 的 native 端到端闭环。
// 观察点全部由夹具自己 printf: 多态 `typeid(*p).name()` 要给出**动态类型**的
// mangled 名 (读的是 vtable 里的 typeinfo 槽), `dynamic_cast` 命中/落空、
// `type_info::operator==` / `.before()` 与 g++ 逐项一致 —— dcc 的 typeinfo
// 对象与 libstdc++ 的 `__class_type_info` 家族二进制同形, 所以
// `__dynamic_cast` 可以直接被 libstdc++ 实现服务。
#include <cstdio>
#include <typeinfo>
struct B { virtual ~B() {} };
struct D : B { virtual void m() {} };
struct U { virtual ~U(){} };
int main(){
  B *p = new D;
  printf("%s\n", typeid(*p).name());
  if (dynamic_cast<D*>(p)) printf("D ok\n");
  if (!dynamic_cast<U*>(p)) printf("U null ok\n");
  const std::type_info& a = typeid(D);
  const std::type_info& b = typeid(*p);
  printf("%d %d\n", a == b, a.name() == b.name());
  delete p;
  B stack_obj;
  printf("base: %s\n", typeid(stack_obj).name());
  printf("before=%d\n", (int)a.before(b));
  return 0;
}
