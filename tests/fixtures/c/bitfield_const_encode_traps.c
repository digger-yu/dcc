/* 内核 include/linux/bitfield.h 的常量位域编码链（真树形状，去掉头文件依赖）。
 * 判据全部取 gcc -O2 实测：三条「值装得下」的调用必须把指向陷阱函数的引用折掉，
 * 而最后一条「值装不下」的必须**留下**引用（反向对照，防止「一律不折」蒙过）。 */
typedef unsigned long long u64;
typedef unsigned int u32;

extern void trap_bad_mask(void);
extern void trap_field_overflow(void);

static __attribute__((always_inline)) inline u64 field_multiplier(u64 field)
{
	if ((field | (field - 1)) & ((field | (field - 1)) + 1))
		trap_bad_mask();
	return field & -field;
}

static __attribute__((always_inline)) inline u64 field_mask(u64 field)
{
	return field / field_multiplier(field);
}

static __attribute__((always_inline)) inline u32 enc(u32 v, u32 field)
{
	if (__builtin_constant_p(v) && (v & ~field_mask(field)))
		trap_field_overflow();
	return (u32)((v & field_mask(field)) * field_multiplier(field));
}

/* 1) 常量实参、装得下（多比特掩码） */
u32 c1_multi(void)
{
	return enc(1023, 0xffff);
}

/* 2) 常量实参、单比特掩码 */
u32 c2_single(void)
{
	return enc(1, 0x80);
}

/* 3) 运行期实参：__builtin_constant_p 为 0，整个检查必须短路掉 */
u32 c3_runtime(u32 x)
{
	return enc(x, 0xffff);
}

/* 4) 反向对照：常量实参**越界**，gcc 也照样发出那条调用 */
u32 c4_overflow_stays(u32 *sink)
{
	u32 r = enc(0x1ff, 0xff);
	*sink = r;
	return r;
}
