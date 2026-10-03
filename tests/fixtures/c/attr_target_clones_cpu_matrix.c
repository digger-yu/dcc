/* attr_target_clones_cpu_matrix.c —— `__builtin_cpu_supports` 的**逐名字**与 gcc 对照。
 *
 * 为什么需要它（`attr_target_clones_cpu_builtin.c` 的四条不变量不够）：那四条钉的是
 * 「非空 + 偏序 + 互斥 + 幂等」，它们能抓恒假桩与「把 leaf7 当 leaf1 读」这类**结构性**
 * 错误，但抓不到「一个名字落在同一 leaf 的另一个位上」—— 例如 RDSEED 曾写成
 * CPUID.(7,0):EBX#22（那是 ENCLV），本机 EBX#22=0 而 #18=1，偏序链里又没有 rdseed，
 * 所以四条不变量全绿而答案是错的。同理，扩展叶（0x8000_0000/0x8000_0001）的探测一旦
 * 永不执行，`lzcnt`/`abm`/`lm` 会**全体静默读 0**，而那四条不变量一条都不碰扩展叶。
 *
 * 本夹具的做法：把 **gcc 13 与 dcc 都接受**的特性名字逐个问一遍（现在是完整的
 * 60 个交集，一个都不排除），全部 `!= 0` 归一（gcc 返回 libgcc 内部位值，C 只承诺
 * 非零 = 支持），然后由判据在四档优化下与 `gcc -O0` 的产物做 stdout **逐字**对照。
 * 于是任何一位写错、任何一条探测被错误门控，都会在这一台真机上显形。
 *
 * 这一轮就是这样抓到第三个位号错的：`movdiri` 曾写成 CPUID.(7,0):**EDX**#27
 * （那一位是 INTEL_STIBP），而权威源是 **(7,0):ECX#27**（Linux word 16
 * `(16*32+27)`、gcc `cpuid.h` 的 `bit_MOVDIRI` 配 `cpuinfo.h` 的 `ecx & bit_MOVDIRI`）。
 * 本机 ECX#27=0 而 EDX#27=1，所以旧实现报「支持 MOVDIRI」而 gcc 报 0。旧版本夹具把
 * `movdiri` **排除**掉，并在注释里把这件事归因于「libgcc 对 >31 的特性字有自己的
 * 存储/版本条件」—— 那个归因是错的，它把实现缺陷掩盖成了外部不可比。现在名字放回
 * 夹具、位号改正，判据同时覆盖两侧。
 *
 * 名字集合的取法：`gcc -w` 与 `dcc` 都必须接受该名字 —— 只在**交集**上比较，才不会有
 * 「一侧编译失败」被当成判据通过。已知不在交集里的：dcc 独有 `cx16`/`fxsr`/`erms`/
 * `pclmulqdq`/`lahf_sahf`/`mmxext`/`prefetchw`/`3dnowprefetch`/`nx`/`syscall`/`rdtscp`/
 * `avx_vnni`（gcc 用 `cmpxchg16b`/`fxsave`/`prfchw`/`lahf_lm`/`avxvnni` 等另一种写法），
 * gcc 独有 `sgx`/`tbm`/`mwaitx`/`clzero` 之外的 AMD 系名字等。
 *
 * 五条机器无关的锚点（除 `sum` 之外还看这五条，所以「全体读 0」不可能靠 gcc 不在场蒙过）：
 *   1) x86-64 恒真：`lm`（来自**扩展叶** 0x8000_0001/EDX#29）、`mmx`、`cmov`、`sse`、`sse2`。
 *      只有 `lm` 这一条能证明扩展叶探测真的跑了。
 *   2) 架构单调偏序：sse2⇒sse、sse3⇒sse2、ssse3⇒sse3、sse4.1⇒ssse3、sse4.2⇒sse4.1、
 *      avx⇒sse4.2、avx2⇒avx、fma⇒avx。
 *   3) `abm` 与 `lzcnt` 是同一个 CPUID 位（0x8000_0001/ECX#5）的两种写法，答案必须相等。
 *   4) 子叶门控的一致性：`xsaveopt`/`xsavec`/`xsaves` 来自 **(0xD,1):EAX**，只有 XSAVE
 *      设施存在时那一子叶才有意义，所以三者任一为真都要求 `xsave` 为真；
 *      `avx512bf16` 来自 **(7,1):EAX** 且要求 XCR0 的 5 个 AVX-512 状态位，所以它
 *      为真必须 `osxsave` 为真。把「子叶不存在」当成「子叶全 0」以外的方向写错
 *      （例如拿 (7,0):EAX 冒充 (7,1):EAX）就会在这一条上露出来。
 *   5) 幂等：同一个名字问两遍必须同答案（缓存与惰性检测不能第二次改写结果）。
 * 任一不成立就返回非 0（gcc 侧同样计算，所以退出码也在对照范围内）。
 */
#include <stdio.h>

int main(void) {
    int v_3dnow = __builtin_cpu_supports("3dnow") != 0;
    int v_abm = __builtin_cpu_supports("abm") != 0;
    int v_adx = __builtin_cpu_supports("adx") != 0;
    int v_aes = __builtin_cpu_supports("aes") != 0;
    int v_avx = __builtin_cpu_supports("avx") != 0;
    int v_avx2 = __builtin_cpu_supports("avx2") != 0;
    int v_avx512bf16 = __builtin_cpu_supports("avx512bf16") != 0;
    int v_avx512bitalg = __builtin_cpu_supports("avx512bitalg") != 0;
    int v_avx512bw = __builtin_cpu_supports("avx512bw") != 0;
    int v_avx512cd = __builtin_cpu_supports("avx512cd") != 0;
    int v_avx512dq = __builtin_cpu_supports("avx512dq") != 0;
    int v_avx512er = __builtin_cpu_supports("avx512er") != 0;
    int v_avx512f = __builtin_cpu_supports("avx512f") != 0;
    int v_avx512ifma = __builtin_cpu_supports("avx512ifma") != 0;
    int v_avx512pf = __builtin_cpu_supports("avx512pf") != 0;
    int v_avx512vbmi = __builtin_cpu_supports("avx512vbmi") != 0;
    int v_avx512vbmi2 = __builtin_cpu_supports("avx512vbmi2") != 0;
    int v_avx512vl = __builtin_cpu_supports("avx512vl") != 0;
    int v_avx512vnni = __builtin_cpu_supports("avx512vnni") != 0;
    int v_avx512vp2intersect = __builtin_cpu_supports("avx512vp2intersect") != 0;
    int v_avx512vpopcntdq = __builtin_cpu_supports("avx512vpopcntdq") != 0;
    int v_bmi = __builtin_cpu_supports("bmi") != 0;
    int v_bmi2 = __builtin_cpu_supports("bmi2") != 0;
    int v_clwb = __builtin_cpu_supports("clwb") != 0;
    int v_clzero = __builtin_cpu_supports("clzero") != 0;
    int v_cmov = __builtin_cpu_supports("cmov") != 0;
    int v_f16c = __builtin_cpu_supports("f16c") != 0;
    int v_fma = __builtin_cpu_supports("fma") != 0;
    int v_gfni = __builtin_cpu_supports("gfni") != 0;
    int v_ibt = __builtin_cpu_supports("ibt") != 0;
    int v_lm = __builtin_cpu_supports("lm") != 0;
    int v_lzcnt = __builtin_cpu_supports("lzcnt") != 0;
    int v_mmx = __builtin_cpu_supports("mmx") != 0;
    int v_movbe = __builtin_cpu_supports("movbe") != 0;
    int v_movdiri = __builtin_cpu_supports("movdiri") != 0;
    int v_osxsave = __builtin_cpu_supports("osxsave") != 0;
    int v_pclmul = __builtin_cpu_supports("pclmul") != 0;
    int v_pku = __builtin_cpu_supports("pku") != 0;
    int v_popcnt = __builtin_cpu_supports("popcnt") != 0;
    int v_rdrnd = __builtin_cpu_supports("rdrnd") != 0;
    int v_rdseed = __builtin_cpu_supports("rdseed") != 0;
    int v_serialize = __builtin_cpu_supports("serialize") != 0;
    int v_sha = __builtin_cpu_supports("sha") != 0;
    int v_shstk = __builtin_cpu_supports("shstk") != 0;
    int v_sse = __builtin_cpu_supports("sse") != 0;
    int v_sse2 = __builtin_cpu_supports("sse2") != 0;
    int v_sse3 = __builtin_cpu_supports("sse3") != 0;
    int v_sse4_1 = __builtin_cpu_supports("sse4.1") != 0;
    int v_sse4_2 = __builtin_cpu_supports("sse4.2") != 0;
    int v_sse4a = __builtin_cpu_supports("sse4a") != 0;
    int v_ssse3 = __builtin_cpu_supports("ssse3") != 0;
    int v_vaes = __builtin_cpu_supports("vaes") != 0;
    int v_vpclmulqdq = __builtin_cpu_supports("vpclmulqdq") != 0;
    int v_waitpkg = __builtin_cpu_supports("waitpkg") != 0;
    int v_wbnoinvd = __builtin_cpu_supports("wbnoinvd") != 0;
    int v_xop = __builtin_cpu_supports("xop") != 0;
    int v_xsave = __builtin_cpu_supports("xsave") != 0;
    int v_xsavec = __builtin_cpu_supports("xsavec") != 0;
    int v_xsaveopt = __builtin_cpu_supports("xsaveopt") != 0;
    int v_xsaves = __builtin_cpu_supports("xsaves") != 0;

    printf("3dnow=%d\n", v_3dnow);
    printf("abm=%d\n", v_abm);
    printf("adx=%d\n", v_adx);
    printf("aes=%d\n", v_aes);
    printf("avx=%d\n", v_avx);
    printf("avx2=%d\n", v_avx2);
    printf("avx512bf16=%d\n", v_avx512bf16);
    printf("avx512bitalg=%d\n", v_avx512bitalg);
    printf("avx512bw=%d\n", v_avx512bw);
    printf("avx512cd=%d\n", v_avx512cd);
    printf("avx512dq=%d\n", v_avx512dq);
    printf("avx512er=%d\n", v_avx512er);
    printf("avx512f=%d\n", v_avx512f);
    printf("avx512ifma=%d\n", v_avx512ifma);
    printf("avx512pf=%d\n", v_avx512pf);
    printf("avx512vbmi=%d\n", v_avx512vbmi);
    printf("avx512vbmi2=%d\n", v_avx512vbmi2);
    printf("avx512vl=%d\n", v_avx512vl);
    printf("avx512vnni=%d\n", v_avx512vnni);
    printf("avx512vp2intersect=%d\n", v_avx512vp2intersect);
    printf("avx512vpopcntdq=%d\n", v_avx512vpopcntdq);
    printf("bmi=%d\n", v_bmi);
    printf("bmi2=%d\n", v_bmi2);
    printf("clwb=%d\n", v_clwb);
    printf("clzero=%d\n", v_clzero);
    printf("cmov=%d\n", v_cmov);
    printf("f16c=%d\n", v_f16c);
    printf("fma=%d\n", v_fma);
    printf("gfni=%d\n", v_gfni);
    printf("ibt=%d\n", v_ibt);
    printf("lm=%d\n", v_lm);
    printf("lzcnt=%d\n", v_lzcnt);
    printf("mmx=%d\n", v_mmx);
    printf("movbe=%d\n", v_movbe);
    printf("movdiri=%d\n", v_movdiri);
    printf("osxsave=%d\n", v_osxsave);
    printf("pclmul=%d\n", v_pclmul);
    printf("pku=%d\n", v_pku);
    printf("popcnt=%d\n", v_popcnt);
    printf("rdrnd=%d\n", v_rdrnd);
    printf("rdseed=%d\n", v_rdseed);
    printf("serialize=%d\n", v_serialize);
    printf("sha=%d\n", v_sha);
    printf("shstk=%d\n", v_shstk);
    printf("sse=%d\n", v_sse);
    printf("sse2=%d\n", v_sse2);
    printf("sse3=%d\n", v_sse3);
    printf("sse4.1=%d\n", v_sse4_1);
    printf("sse4.2=%d\n", v_sse4_2);
    printf("sse4a=%d\n", v_sse4a);
    printf("ssse3=%d\n", v_ssse3);
    printf("vaes=%d\n", v_vaes);
    printf("vpclmulqdq=%d\n", v_vpclmulqdq);
    printf("waitpkg=%d\n", v_waitpkg);
    printf("wbnoinvd=%d\n", v_wbnoinvd);
    printf("xop=%d\n", v_xop);
    printf("xsave=%d\n", v_xsave);
    printf("xsavec=%d\n", v_xsavec);
    printf("xsaveopt=%d\n", v_xsaveopt);
    printf("xsaves=%d\n", v_xsaves);

    /* 一个聚合读数：任何一位变动都会同时改动 per-line 与这里的和（便于人读 diff）。 */
    int sum = v_3dnow + v_abm + v_adx + v_aes + v_avx + v_avx2 + v_avx512bf16
            + v_avx512bitalg + v_avx512bw + v_avx512cd + v_avx512dq + v_avx512er
            + v_avx512f + v_avx512ifma + v_avx512pf + v_avx512vbmi + v_avx512vbmi2
            + v_avx512vl + v_avx512vnni + v_avx512vp2intersect + v_avx512vpopcntdq
            + v_bmi + v_bmi2 + v_clwb + v_clzero + v_cmov + v_f16c + v_fma + v_gfni
            + v_ibt + v_lm + v_lzcnt + v_mmx + v_movbe + v_movdiri + v_osxsave
            + v_pclmul + v_pku + v_popcnt + v_rdrnd + v_rdseed + v_serialize + v_sha
            + v_shstk + v_sse + v_sse2 + v_sse3 + v_sse4_1 + v_sse4_2 + v_sse4a
            + v_ssse3 + v_vaes + v_vpclmulqdq + v_waitpkg + v_wbnoinvd + v_xop
            + v_xsave + v_xsavec + v_xsaveopt + v_xsaves;
    printf("sum=%d\n", sum);

    int bad = 0;
    /* 1) x86-64 架构恒真（lm 只在扩展叶探测真的跑了才会是 1） */
    if (!v_lm) bad++;
    if (!v_mmx || !v_cmov || !v_sse || !v_sse2) bad++;
    /* 2) 架构单调偏序 */
    if (v_sse3 && !v_sse2) bad++;
    if (v_ssse3 && !v_sse3) bad++;
    if (v_sse4_1 && !v_ssse3) bad++;
    if (v_sse4_2 && !v_sse4_1) bad++;
    if (v_avx && !v_sse4_2) bad++;
    if (v_avx2 && !v_avx) bad++;
    if (v_fma && !v_avx) bad++;
    /* 3) 同一个 CPUID 位的两种写法必须同答案 */
    if (v_abm != v_lzcnt) bad++;
    /* 4) 子叶门控一致性 */
    if ((v_xsaveopt || v_xsavec || v_xsaves) && !v_xsave) bad++;
    if (v_avx512bf16 && !v_osxsave) bad++;
    /* 5) 幂等：再问一遍必须同答案 */
    if ((__builtin_cpu_supports("sse4.2") != 0) != v_sse4_2) bad++;
    if ((__builtin_cpu_supports("avx512bf16") != 0) != v_avx512bf16) bad++;
    return bad ? 7 : 0;
}
