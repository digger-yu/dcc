// 复杂数学 cproj / cabs / carg libm forwarding fixture (Section 11 P0.3).
// 注: dcc 当前 _Complex 解析路径未实现 (Section 9 follow-up). 本 fixture
// 显式声明 cproj / cabs / carg 入口, 调用并验证 .o 引用. 运行时签名
// 不严格匹配 glibc libm (glibc 用 _Complex / struct 传值), 但
// host gcc -lm 链接时通过符号解析让 dcc 编译出的 .o 仍能链接成功.
// 真实数学校验留给 Section 9 follow-up (struct 传值 ABI 修复后).
double cproj(double complex_v);
double cabs(double complex_v);
double carg(double complex_v);

int main(void) {
    double v;
    v = cproj(1.0);
    v += cabs(1.0);
    v += carg(1.0);
    (void)v;
    return 0;
}
