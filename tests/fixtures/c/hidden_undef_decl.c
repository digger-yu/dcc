/* 未被引用的 `visibility("hidden")` 声明不得变成 UND 条目。
 *
 * 背景（`report/linux71-dcc-fulltree-build-2026-10-28.md` §3.25）：内核
 * `include/vdso/datapage.h:161` 有
 *     extern struct vdso_arch_data vdso_u_arch_data __attribute__((visibility("hidden")));
 * 在 x86 的 `drivers/char/random.c` 里**只是声明**（用户是 riscv/loongarch），dcc 以前按
 * 「文件作用域的无定义声明」逐条登记 UND ⇒ 最终链接直接硬错
 *     ld: .tmp_vmlinux1: hidden symbol 'vdso_u_arch_data' isn't defined
 * 因为 ld 对**未定义的隐藏符号**是错误而非警告（普通未定义符号还能等别的 TU 供定义）。
 *
 * 判据为什么只针对 hidden（而不是「所有未被引用的 UND」）：宽版实测会拆掉
 * `-fbuiltin-linker` 那条偶然的安全网（幽灵 UND 消失后，内置链接器不再报
 * `symbol 'printf': undefined reference`，转而链出一个**段错误**的静态 ET_EXEC），
 * 还推翻既有契约用例 `attr_malloc_family_decls_keep_external_linkage`。宽版留待
 * 单独处理（见报告同一节），这里只锁内核真正被挡住的那一条。
 *
 * `use_other` 用来证明「被引用的一律保留」不被误伤。
 */
struct vdso_arch_data {
    long cpu_rng_seed_sizes;
};

extern struct vdso_arch_data vdso_u_arch_data __attribute__((visibility("hidden")));
extern int use_other(void);

int probe(void)
{
    return use_other();
}
