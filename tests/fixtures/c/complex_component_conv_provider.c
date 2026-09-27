/* _Complex float / _Complex double 的跨 TU 互操作夹具: 生产者侧。
 *
 * 本文件只导出函数与全局对象, 没有 main —— 消费者侧
 * complex_component_conv_consumer.c 调用它们并打印分量。
 * 两侧可以被 gcc 与 dcc 任意组合编译后链接 (SysV ABI 3.2.3:
 * `_Complex float` 是一个 [Sse] eightbyte (实部/虚部各 4 字节打包),
 * `_Complex double` 是 [Sse, Sse] 两个 eightbyte), 因而这里的每个函数
 * 都是对"另一半由外部实现负责"的真实互操作面。
 */

/* 恒等: 只搬运, 检验 caller/callee 对打包方式的约定是否一致 */
_Complex float cf_id(_Complex float z) {
    return z;
}

_Complex double cd_id(_Complex double z) {
    return z;
}

/* 分量转换 float <-> double: 每个分量单独缩放后重新打包 */
_Complex double cf_to_cd(_Complex float z) {
    _Complex double r;
    __real__ r = (double)__real__ z * 2.0;
    __imag__ r = (double)__imag__ z * 3.0;
    return r;
}

_Complex float cd_to_cf(_Complex double z) {
    _Complex float r;
    __real__ r = (float)(__real__ z + __imag__ z);
    __imag__ r = (float)(__real__ z - __imag__ z);
    return r;
}

/* 整型 -> 复数的隐式转换 (虚部清零) */
_Complex double int_to_cd(int n) {
    return n;
}

_Complex float int_to_cf(int n) {
    return n;
}

/* 复数算术: 分量各自乘加 */
_Complex float cf_scale(_Complex float z, float k) {
    return z * k;
}

_Complex double cd_add(_Complex double a, _Complex double b) {
    return a + b;
}

/* 7 个整型形参之后接 `_Complex double`: GP 寄存器耗尽前把 SSE 对
 * 放错位置 (或错当成一个 eightbyte) 会在这里暴露。 */
_Complex double cd_after_seven_ints(int a, int b, int c, int d,
                                    int e, int f, int g,
                                    _Complex double z) {
    _Complex double r;
    __real__ r = __real__ z + (a + b + c + d + e + f + g);
    __imag__ r = __imag__ z - (a + b + c + d + e + f + g);
    return r;
}

/* 复数与标量混合类别: [Sse,Sse] + [Sse] + [Integer] */
long cd_mixed(_Complex double z, double k, long tag) {
    double re = __real__ z * k;
    double im = __imag__ z * k;
    return (long)(re + im) + tag;
}

/* 复数入参在函数内被取地址: 强制 callee 把 XMM 里的打包值 spill 成
 * 内存对象, 再按分量读出 —— 打包顺序错一位就会给出交换后的分量。 */
_Complex double cd_spill_readback(_Complex double z) {
    _Complex double *p = &z;
    _Complex double r;
    __real__ r = __real__ *p * 2.0;
    __imag__ r = __imag__ *p * 2.0;
    return r;
}

_Complex float cf_spill_readback(_Complex float z) {
    _Complex float *p = &z;
    _Complex float r;
    __real__ r = __real__ *p + __imag__ *p;
    __imag__ r = __real__ *p - __imag__ *p;
    return r;
}

/* 跨 TU 的全局复数数组: 生产者定义, 消费者按元素读分量 */
_Complex double g_cd[4] = {1.5 + 2.5i, 3.25 + 4.5i, -5.5 + 6.75i, 7.5 - 8.25i};
_Complex float g_cf[3] = {0.5 + 1.25i, 2.5 - 3.5i, 4.5 + 5.75i};

/* 返回复数的间接路径: 函数指针, 检验返回值类别不被猜测成 Memory */
typedef _Complex double (*cd_fn)(double _Complex);

cd_fn cd_selector(void) {
    return cd_id;
}
