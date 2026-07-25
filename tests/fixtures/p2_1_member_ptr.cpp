// P2.1: obj.*ptr_to_member 和 obj->*ptr_to_member 真解引用
// (Section 6 row, M17.0 base work + P2.1 解析器修补).
// 验证 dcc 解析 + codegen 路径:
//   - p.*x 走 MemberStar 路径, 编译为
//     (*(int*)((char*)(p) + __dcc_mbr_Point_x))
//   - pp->*x 走 PtrMemberStar 路径.
struct Point {
    int x;
    int y;
};

int test_dotstar() {
    Point p;
    p.x = 3;
    p.y = 4;
    // p.*x 和 p.*y 走 MemberStar, 用 __dcc_mbr_Point_x/y 偏移
    int a = p.*x;
    int b = p.*y;
    return a + b;
}

int test_arrowstar() {
    Point p;
    p.x = 5;
    p.y = 6;
    Point* pp = &p;
    // pp->*x 和 pp->*y 走 PtrMemberStar
    int a = pp->*x;
    int b = pp->*y;
    return a + b;
}
