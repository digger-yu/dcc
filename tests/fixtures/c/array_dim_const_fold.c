// 数组维度整型常量表达式折叠: 维度必须是编译期整型常量表达式,
// `BITS_PER_LONG + 1` 应折叠为 65 而非保留为 VLA 表达式.
// 内核 kernel/cpu.c:
//   const unsigned long cpu_bit_bitmap[BITS_PER_LONG+1][BITS_TO_LONGS(NR_CPUS)]
//     = { ... };
//   EXPORT_SYMBOL_GPL(cpu_bit_bitmap);  // -> extern typeof(cpu_bit_bitmap) ...
// 修复前维度是 ArraySize::Var, 其相等比较恒 false, typeof 重声明
// 类型不匹配导致误报 E0007.

#define BITS_PER_LONG 64
#define BITS_TO_LONGS(nr) (((nr) + BITS_PER_LONG - 1) / BITS_PER_LONG)
#define NR_CPUS 512

const unsigned long cpu_bit_bitmap[BITS_PER_LONG + 1][BITS_TO_LONGS(NR_CPUS)] = {
    [0 ... BITS_PER_LONG] = { [0 ... BITS_TO_LONGS(NR_CPUS) - 1] = 0UL },
};

extern typeof(cpu_bit_bitmap) cpu_bit_bitmap;
