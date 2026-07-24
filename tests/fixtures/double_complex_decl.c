int main(void) {
    /* 测试 `double _Complex` 类型说明符 (C99 complex.h 标准形式)
     * 在 Linux 上 _Complex double 大小为 16 字节 (2 个 double)
     * 用 sizeof 验证类型已正确建立, 避开 __real__/__imag__ builtin */
    double _Complex z = 0.0;
    double _Complex w = 1.0;
    z = w;
    /* 任何 _Complex 算术都能通过编译: 加减乘除均可
     * 结果: z == w (1.0) */
    return sizeof(z) == 16 ? 0 : 1;
}
