// P2.1: obj.*ptr_to_member 和 obj->*ptr_to_member 真解引
// M27: 解引类型不再硬编码 int*, 而是查找字段真实类型
// 验证 dcc lower 阶段为 MemberStar/PtrMemberStar 生成
// 正确类型的 deref (如 double* 而非 int*)
struct Point {
    int x;
    double y;
};

int test_dotstar_int() {
    Point p;
    p.x = 3;
    // p.*x 走 MemberStar, 解引类型应为 int*
    int a = p.*x;
    return a;  // 期望 3
}

int test_dotstar_double() {
    Point p;
    p.y = 4.5;
    // p.*y 走 MemberStar, 解引类型应为 double*
    // 赋值给 int 会截断, 验证类型正确
    double b = p.*y;
    return (int)b;  // 期望 4
}

int test_arrowstar() {
    Point p;
    p.x = 5;
    p.y = 6.5;
    Point* pp = &p;
    // pp->*x 和 pp->*y 走 PtrMemberStar
    int a = pp->*x;
    double b = pp->*y;
    return a + (int)b;  // 期望 11
}
