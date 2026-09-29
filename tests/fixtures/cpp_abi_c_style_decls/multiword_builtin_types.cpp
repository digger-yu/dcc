// [dcl.type.simple]: **多词基本类型说明符**必须按「符号性 / 长度 / 基准类型」
// 三个维度归一, 且与词序无关。
//
// 早前 C++ 前端按拼写字符串枚举组合, 未命中的写法 (实测 `signed short int`、
// `signed long long int`) 会降级成 `MangleType::Class("signed short int")`,
// sema 报 `unknown class type: signed short int` —— GCC 15.3.0 `libcpp/*.cc`
// 的 16 个 TU 全部因此退回 legacy 路径。
//
// 交叉验证口径: 每种写法的 `sizeof` / 对齐 / 有符号性 / 算术行为都要与 g++ 一致。
#include <cstdio>

// 各种等价的拼写形态混用在同一份声明里
signed short int ss_var;
short int si_var;
signed signedness_check;
unsigned short int us_var;
signed long int sl_var;
signed long long int sll_var;
unsigned long long int ull_var;
long double ld_var;
unsigned char uc_var;
signed char sc_var;

// 词序打乱 (GNU 允许, 含义不变)
int short is_var;
long long signed isll_var;

struct UsesAll {
    signed short int a;
    unsigned long long b;
    long long int c;
};

// 对齐由相邻成员间的填充体现 (不用 alignof: 系统头把它预展开成 `_Alignof`,
// 而那是 C 的关键字, 不在本夹具的验证范围内)
struct PadShort {
    char c;
    signed short int s;
};

struct PadWide {
    char c;
    unsigned long long int l;
};

long arith(long x) {
    signed short int y = (signed short int)x;
    unsigned short int z = (unsigned short int)x;
    return (long)y + (long)z;
}

int main() {
    printf("sizes %zu %zu %zu %zu %zu %zu %zu %zu\n",
           sizeof(ss_var), sizeof(signedness_check), sizeof(us_var), sizeof(sl_var),
           sizeof(sll_var), sizeof(ull_var), sizeof(ld_var), sizeof(sc_var));
    PadShort ps;
    PadWide pw;
    printf("padding size=%zu,%zu off=%td,%td\n",
           sizeof(PadShort), sizeof(PadWide),
           (char*)&ps.s - (char*)&ps, (char*)&pw.l - (char*)&pw);

    // 有符号性: 最负值与最高位行为
    ss_var = -1;
    printf("ss=%d us=%d\n", (int)ss_var, (int)us_var);
    sc_var = -1;
    uc_var = 255;
    printf("char signed=%d unsigned=%d plain=%d\n",
           (int)sc_var, (int)uc_var, (int)(char)-1);

    sll_var = 1;
    sll_var <<= 62;
    ull_var = 1;
    ull_var <<= 63;
    printf("wide %lld %llu\n", sll_var, ull_var);

    is_var = 3;
    isll_var = 4;
    printf("shuffled %d %lld\n", (int)is_var, isll_var);

    UsesAll u;
    u.a = -2;
    u.b = 5;
    u.c = -6;
    printf("struct size=%zu a=%d b=%llu c=%lld\n",
           sizeof(u), (int)u.a, u.b, u.c);

    printf("arith=%ld\n", arith(-1000));

    // long double 的算术精度
    ld_var = 1.0L / 3.0L;
    printf("ld=%.6Lf size=%zu\n", ld_var, sizeof(ld_var));
    return (int)(sizeof(ss_var) * 2 + sizeof(sll_var) / 4);
}
