/* [Sse, SseUp] 双 eightbyte 只能消耗一个 16 字节 XMM 槽。
 *
 * 覆盖缺陷 D2：寄存器保存区路径曾把 SseUp 当成独立槽，对 16 字节向量做
 * `fp_offset += 32`。后果是从第二个向量参数起全部读错槽位——但值仍"看起来
 * 像浮点"，`assert_eq!(code, 0)` 型断言看不见，必须靠与 gcc 同源差分。
 *
 * 1.5/2.5/3.5/4.5 都可在 binary64 中精确表示，比较不引入舍入差异。
 */
typedef __builtin_va_list va_list;
typedef double v2df __attribute__((vector_size(16)));

static int probe(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    v2df a = __builtin_va_arg(ap, v2df); /* [Sse, SseUp] → 1 个槽 */
    v2df b = __builtin_va_arg(ap, v2df); /* 必须是紧接着的下一个槽 */
    __builtin_va_end(ap);
    int r = 0;
    if (a[0] != 1.5 || a[1] != 2.5) r |= 1;
    if (b[0] != 3.5 || b[1] != 4.5) r |= 2;
    return r;
}

int main(void) {
    v2df x, y;
    x[0] = 1.5;
    x[1] = 2.5;
    y[0] = 3.5;
    y[1] = 4.5;
    return probe(0, x, y);
}
