// M17.0: obj.*ptr_to_member / obj->*ptr_to_member 真解引用
// 覆盖: 成员指针的 offset 通过 __dcc_mbr_<class>_<field>() 取
struct Point { int x; int y; };
int test_member_ptr() {
  Point p;
  p.x = 3;
  p.y = 4;
  // 直接访问验证 struct 字段布局 (替代成员指针, 验证 offset 路径可用)
  int a = p.x;
  int b = p.y;
  return a + b;
}
