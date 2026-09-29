// C 头文件里最常见的类型定义形态: **匿名 struct/union 的 typedef**。
//
// [dcl.type.anon]/1 + [dcl.typedef]/1: 匿名聚合体没有 tag, 它的名字完全由
// typedef 赋予。glibc `<stdio.h>` 的 `typedef struct { int __val[2]; }
// _G_fpos_t;` 就是这个形态; 早前 dcc 的 C++ 前端把成员体整段跳过、只留一个
// `Typedef{target: int}` 占位, 于是字段全丢, 使用点报
// `unknown class type: _G_fpos_t` —— GCC 15.3.0 `libcpp/*.cc` 16 个 TU 全部
// 卡在这里退回 legacy 路径。
//
// 这里把「字段真的存在、偏移真的对、尺寸真的对」全部打印出来与 g++ 对照。
#include <cstdio>

typedef struct {
    int __val[2];
    char tail;
} fpos_t_like;

typedef union {
    int i;
    float f;
    long l;
} scalar_union;

// 带 tag 的 typedef: tag 与别名都必须是可用类型
typedef struct Tagged {
    int a;
    char b;
    long c;
} Name;

// 一条 typedef 带多个声明符 (含指针别名)
typedef struct {
    int x;
    int y;
} Point, *PointPtr;

// 匿名定义 + 变量实例 (非 typedef), 与上面的形态共用聚合体解析
union {
    int as_int;
    char as_bytes[4];
} punned;

int main() {
    fpos_t_like fp;
    fp.__val[0] = 11;
    fp.__val[1] = 22;
    fp.tail = 33;
    printf("fpos size=%zu val=%d,%d tail=%d\n",
           sizeof(fpos_t_like), fp.__val[0], fp.__val[1], (int)fp.tail);
    printf("fpos offsets val0=%td val1=%td tail=%td\n",
           (char*)&fp.__val[0] - (char*)&fp,
           (char*)&fp.__val[1] - (char*)&fp,
           (char*)&fp.tail - (char*)&fp);

    scalar_union u;
    u.i = 0;
    printf("union size=%zu i=%d\n", sizeof(scalar_union), u.i);
    u.f = 1.5f;
    printf("union f=%.1f\n", (double)u.f);

    Name n;
    n.a = 7;
    n.b = 8;
    n.c = 9;
    Tagged t2;
    t2.a = 70;
    printf("name=%d,%d,%d tagged=%d size=%zu\n",
           n.a, (int)n.b, (int)n.c, t2.a, sizeof(Name));

    Point p;
    p.x = 1;
    p.y = 2;
    PointPtr pp = &p;
    pp->x = 100;
    printf("point=%d,%d\n", p.x, p.y);

    punned.as_int = 0x41424344;
    printf("punned bytes=%d,%d,%d,%d size=%zu\n",
           punned.as_bytes[0], punned.as_bytes[1],
           punned.as_bytes[2], punned.as_bytes[3], sizeof(punned));
    return 0;
}
