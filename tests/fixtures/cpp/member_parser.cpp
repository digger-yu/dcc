// m26_member_parser.cpp - 类成员 / 字面量 / 运算符名解析增强
// 验证: 引用成员类型保留 + operator new/delete([]) + hex/oct/bin/
// 后缀整数 + 转义字符 + 浮点字面量. 运行返回 0 表示全部断言通过.

class Widget {
public:
    int id;
    Widget() {}
};

class Holder {
public:
    Widget& ref;          // 引用成员 (类型保留, 降级为指针)
    void* operator new(unsigned long sz) { (void)sz; return 0; }
    void operator delete(void* p) { (void)p; }
    int* operator new[](unsigned long sz) { (void)sz; return 0; }
    void operator delete[](void* p) { (void)p; }
};

int main() {
    int r = 0;
    int hex = 0x41;      // 65
    int oct = 0755;      // 493
    int bin = 0b1010;    // 10
    int big = 42ULL;     // 42 (带后缀)
    char nl = '\n';      // 10
    char hx = '\x41';    // 65
    double d = 1.5;      // 浮点字面量
    if (hex != 65) r |= 1;
    if (oct != 493) r |= 2;
    if (bin != 10) r |= 4;
    if (big != 42) r |= 8;
    if (nl != 10) r |= 16;
    if (hx != 65) r |= 32;
    if (d < 1.0) r |= 64;
    return r;
}
