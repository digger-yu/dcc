/* XMM 寄存器**个数**（不是 eightbyte 个数）决定 fp_offset 的上限与推进。
 *
 * 覆盖缺陷 D3：`gen_va_arg_struct` 三副本曾按「SSE 家族 eightbyte 的个数」算
 * `n_fp`，于是 16 字节向量 `[Sse, SseUp]` 被当成消耗 2 个 XMM 寄存器，上限从
 * `176 - 16 = 160` 缩到 `176 - 32 = 144`。后果是第 8 个向量参数（读前
 * `fp_offset = 48 + 7*16 = 160`）被误判为寄存器保存区已耗尽、改从
 * `overflow_arg_area` 取，而 caller 实际把它留在了 `xmm7` 的保存槽里——从第 8
 * 个起全部错位。
 *
 * 8 个 XMM 寄存器是硬上限，所以第 9/10 个必须走溢出区；这条同时约束
 * 保存区路径与溢出路径的交接点。返回值是「第一个读错的参数序号」，0 = 全对，
 * 使故障位置可见（`assert_eq!(code, 0)` 型断言只看得见对错，看不见偏移）。
 *
 * 所有值都是小整数值的 binary64，可精确表示，比较不引入舍入差异。
 */
typedef __builtin_va_list va_list;
typedef double v2df __attribute__((vector_size(16)));

static int probe(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    for (int i = 0; i < 10; i++) {
        v2df v = __builtin_va_arg(ap, v2df); /* [Sse, SseUp] → 1 个 xmm 寄存器 */
        if (v[0] != (double)(i * 2 + 1) || v[1] != (double)(i * 2 + 2))
            return i + 1;
    }
    __builtin_va_end(ap);
    return 0;
}

int main(void) {
    return probe(0,
                 (v2df){1, 2},
                 (v2df){3, 4},
                 (v2df){5, 6},
                 (v2df){7, 8},
                 (v2df){9, 10},
                 (v2df){11, 12},
                 (v2df){13, 14},
                 (v2df){15, 16},
                 (v2df){17, 18},
                 (v2df){19, 20});
}
