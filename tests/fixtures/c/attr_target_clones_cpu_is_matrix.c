/* attr_target_clones_cpu_is_matrix.c —— `__builtin_cpu_is` 的**型号名**与 gcc 对照。
 *
 * 为什么需要它（`attr_target_clones_cpu_builtin.c` 不够）：那个夹具只问
 * `intel`/`amd` 两个厂商名，它证明的是「leaf0 厂商 dword 被认出来了」。而
 * `__builtin_cpu_is` 真正的难点在**微架构型号名**：它要求 leaf1 EAX 的
 * family/model 解码 + gcc 那张按厂商/family 分组、按 model 逐条、必要时还要
 * 用特性位当守卫的分类表。这一层以前根本不存在（型号名直接报 E1125），
 * 所以必须有一个「逐个名字问一遍、与 gcc 逐字对照」的判据来钉它。
 *
 * 名字集合：本夹具用的就是 gcc 13.3 实际接受的**全部 51 个**名字（逐个实测：
 * `athlon`/`pentium4`/`k6`/`amdfam10`/`znver5`/`amdfam1ah`/`generic`/`centaur`/
 * `zhaoxin` 在 gcc 13.3 里都是编译错误，dcc 也必须报错，所以不放在这里）。
 * 判据在四档优化下把 stdout 与 `gcc -O0` 的产物**逐字**比对 —— 任何一条分类
 * 规则写错（解码掩码、组顺序、首条命中即止、守卫位号）都会在这一台真机上显形。
 * 本机（Intel family 6 model 0xa7）gcc 的实测答案是 `intel=1 corei7=1 rocketlake=1`，
 * 其余 48 个名字全是 0。
 *
 * 下面五条机器无关锚点（与本机是什么 CPU 无关，恒真）用来防「全体静默答 0」
 * 这一类蒙对：
 *   1) 厂商互斥：`intel` 与 `amd` 不能同时为真。
 *   2) `atom` 与 `bonnell` 必须永远同答案 —— gcc 的表里两个名字都指向同一个
 *      `INTEL_BONNELL`，dcc 的 `CPU_IS_NAMES` 里两者都是 `Type("bonnell")`，
 *      比较同一个 dword。这条把「一个名字比多个 dword」这种自作主张钉掉。
 *   3) 子型 ⇒ 父型：`rocketlake ⇒ corei7`、`znver1 ⇒ amdfam17h`、
 *      `bdver3 ⇒ amdfam15h`、`barcelona ⇒ amdfam10h`（分类表里同一组写的）。
 *   4) 型号 ⇒ 厂商：Intel 的型号名都在 `vendor == INTEL` 的组里，AMD 同理。
 *   5) 至多一个子型为真：`subtype_id` 每个名字唯一，而分类链只写一次
 *      `SLOT_CPU_SUBTYPE` 就结束整条链 —— 数出两个子型名同时为真说明槽被写花
 *      或者比较端读错了槽。
 * 任一不成立就返回非 0（gcc 侧同样计算，所以退出码也在对照范围内）。
 */
#include <stdio.h>

int main(void) {
    /* ---- 厂商名 ---- */
    int intel = __builtin_cpu_is("intel") != 0;
    int amd = __builtin_cpu_is("amd") != 0;

    /* ---- 类型名（比 __cpu_type）---- */
    int amdfam10h = __builtin_cpu_is("amdfam10h") != 0;
    int amdfam15h = __builtin_cpu_is("amdfam15h") != 0;
    int amdfam17h = __builtin_cpu_is("amdfam17h") != 0;
    int amdfam19h = __builtin_cpu_is("amdfam19h") != 0;
    int btver1 = __builtin_cpu_is("btver1") != 0;
    int btver2 = __builtin_cpu_is("btver2") != 0;
    int core2 = __builtin_cpu_is("core2") != 0;
    int corei7 = __builtin_cpu_is("corei7") != 0;
    int bonnell = __builtin_cpu_is("bonnell") != 0;
    int atom = __builtin_cpu_is("atom") != 0;
    int silvermont = __builtin_cpu_is("silvermont") != 0;
    int goldmont = __builtin_cpu_is("goldmont") != 0;
    int goldmont_plus = __builtin_cpu_is("goldmont-plus") != 0;
    int tremont = __builtin_cpu_is("tremont") != 0;
    int knl = __builtin_cpu_is("knl") != 0;
    int knm = __builtin_cpu_is("knm") != 0;
    int sierraforest = __builtin_cpu_is("sierraforest") != 0;
    int grandridge = __builtin_cpu_is("grandridge") != 0;

    /* ---- 子型名（比 __cpu_subtype）---- */
    int barcelona = __builtin_cpu_is("barcelona") != 0;
    int shanghai = __builtin_cpu_is("shanghai") != 0;
    int istanbul = __builtin_cpu_is("istanbul") != 0;
    int bdver1 = __builtin_cpu_is("bdver1") != 0;
    int bdver2 = __builtin_cpu_is("bdver2") != 0;
    int bdver3 = __builtin_cpu_is("bdver3") != 0;
    int bdver4 = __builtin_cpu_is("bdver4") != 0;
    int znver1 = __builtin_cpu_is("znver1") != 0;
    int znver2 = __builtin_cpu_is("znver2") != 0;
    int znver3 = __builtin_cpu_is("znver3") != 0;
    int znver4 = __builtin_cpu_is("znver4") != 0;
    int lujiazui = __builtin_cpu_is("lujiazui") != 0;
    int nehalem = __builtin_cpu_is("nehalem") != 0;
    int westmere = __builtin_cpu_is("westmere") != 0;
    int sandybridge = __builtin_cpu_is("sandybridge") != 0;
    int ivybridge = __builtin_cpu_is("ivybridge") != 0;
    int haswell = __builtin_cpu_is("haswell") != 0;
    int broadwell = __builtin_cpu_is("broadwell") != 0;
    int skylake = __builtin_cpu_is("skylake") != 0;
    int skylake_avx512 = __builtin_cpu_is("skylake-avx512") != 0;
    int cascadelake = __builtin_cpu_is("cascadelake") != 0;
    int cooperlake = __builtin_cpu_is("cooperlake") != 0;
    int cannonlake = __builtin_cpu_is("cannonlake") != 0;
    int icelake_client = __builtin_cpu_is("icelake-client") != 0;
    int icelake_server = __builtin_cpu_is("icelake-server") != 0;
    int tigerlake = __builtin_cpu_is("tigerlake") != 0;
    int rocketlake = __builtin_cpu_is("rocketlake") != 0;
    int alderlake = __builtin_cpu_is("alderlake") != 0;
    int sapphirerapids = __builtin_cpu_is("sapphirerapids") != 0;
    int graniterapids = __builtin_cpu_is("graniterapids") != 0;
    int graniterapids_d = __builtin_cpu_is("graniterapids-d") != 0;

    printf("intel=%d\n", intel);
    printf("amd=%d\n", amd);
    printf("amdfam10h=%d\n", amdfam10h);
    printf("amdfam15h=%d\n", amdfam15h);
    printf("amdfam17h=%d\n", amdfam17h);
    printf("amdfam19h=%d\n", amdfam19h);
    printf("btver1=%d\n", btver1);
    printf("btver2=%d\n", btver2);
    printf("core2=%d\n", core2);
    printf("corei7=%d\n", corei7);
    printf("bonnell=%d\n", bonnell);
    printf("atom=%d\n", atom);
    printf("silvermont=%d\n", silvermont);
    printf("goldmont=%d\n", goldmont);
    printf("goldmont-plus=%d\n", goldmont_plus);
    printf("tremont=%d\n", tremont);
    printf("knl=%d\n", knl);
    printf("knm=%d\n", knm);
    printf("sierraforest=%d\n", sierraforest);
    printf("grandridge=%d\n", grandridge);
    printf("barcelona=%d\n", barcelona);
    printf("shanghai=%d\n", shanghai);
    printf("istanbul=%d\n", istanbul);
    printf("bdver1=%d\n", bdver1);
    printf("bdver2=%d\n", bdver2);
    printf("bdver3=%d\n", bdver3);
    printf("bdver4=%d\n", bdver4);
    printf("znver1=%d\n", znver1);
    printf("znver2=%d\n", znver2);
    printf("znver3=%d\n", znver3);
    printf("znver4=%d\n", znver4);
    printf("lujiazui=%d\n", lujiazui);
    printf("nehalem=%d\n", nehalem);
    printf("westmere=%d\n", westmere);
    printf("sandybridge=%d\n", sandybridge);
    printf("ivybridge=%d\n", ivybridge);
    printf("haswell=%d\n", haswell);
    printf("broadwell=%d\n", broadwell);
    printf("skylake=%d\n", skylake);
    printf("skylake-avx512=%d\n", skylake_avx512);
    printf("cascadelake=%d\n", cascadelake);
    printf("cooperlake=%d\n", cooperlake);
    printf("cannonlake=%d\n", cannonlake);
    printf("icelake-client=%d\n", icelake_client);
    printf("icelake-server=%d\n", icelake_server);
    printf("tigerlake=%d\n", tigerlake);
    printf("rocketlake=%d\n", rocketlake);
    printf("alderlake=%d\n", alderlake);
    printf("sapphirerapids=%d\n", sapphirerapids);
    printf("graniterapids=%d\n", graniterapids);
    printf("graniterapids-d=%d\n", graniterapids_d);

    /* 聚合读数：任何一次分类变动都会同时改动 per-line 与这里的和。 */
    int sum = intel + amd + amdfam10h + amdfam15h + amdfam17h + amdfam19h
            + btver1 + btver2 + core2 + corei7 + bonnell + atom + silvermont
            + goldmont + goldmont_plus + tremont + knl + knm + sierraforest
            + grandridge + barcelona + shanghai + istanbul + bdver1 + bdver2
            + bdver3 + bdver4 + znver1 + znver2 + znver3 + znver4 + lujiazui
            + nehalem + westmere + sandybridge + ivybridge + haswell + broadwell
            + skylake + skylake_avx512 + cascadelake + cooperlake + cannonlake
            + icelake_client + icelake_server + tigerlake + rocketlake
            + alderlake + sapphirerapids + graniterapids + graniterapids_d;
    printf("sum=%d\n", sum);

    int bad = 0;
    /* 1) 厂商互斥 */
    if (intel && amd) bad++;
    /* 2) 同 id 的两个名字必须永远同答案 */
    if (atom != bonnell) bad++;
    /* 3) 子型 ⇒ 父型（同一分类组写的类型） */
    if (nehalem && !corei7) bad++;
    if (westmere && !corei7) bad++;
    if (sandybridge && !corei7) bad++;
    if (ivybridge && !corei7) bad++;
    if (haswell && !corei7) bad++;
    if (broadwell && !corei7) bad++;
    if (skylake && !corei7) bad++;
    if (skylake_avx512 && !corei7) bad++;
    if (cascadelake && !corei7) bad++;
    if (cooperlake && !corei7) bad++;
    if (cannonlake && !corei7) bad++;
    if (icelake_client && !corei7) bad++;
    if (icelake_server && !corei7) bad++;
    if (tigerlake && !corei7) bad++;
    if (rocketlake && !corei7) bad++;
    if (alderlake && !corei7) bad++;
    if (sapphirerapids && !corei7) bad++;
    if (graniterapids && !corei7) bad++;
    if (graniterapids_d && !corei7) bad++;
    if (barcelona && !amdfam10h) bad++;
    if (shanghai && !amdfam10h) bad++;
    if (istanbul && !amdfam10h) bad++;
    if (bdver1 && !amdfam15h) bad++;
    if (bdver2 && !amdfam15h) bad++;
    if (bdver3 && !amdfam15h) bad++;
    if (bdver4 && !amdfam15h) bad++;
    if (znver1 && !amdfam17h) bad++;
    if (znver2 && !amdfam17h) bad++;
    if (znver3 && !amdfam19h) bad++;
    if (znver4 && !amdfam19h) bad++;
    /* 4) 型号 ⇒ 厂商 */
    if ((core2 || corei7 || bonnell || silvermont || goldmont || goldmont_plus
         || tremont || knl || knm || sierraforest || grandridge) && !intel) bad++;
    if ((amdfam10h || amdfam15h || amdfam17h || amdfam19h || btver1 || btver2) && !amd) bad++;
    /* 5) 至多一个子型为真 */
    int subs = barcelona + shanghai + istanbul + bdver1 + bdver2 + bdver3 + bdver4
             + znver1 + znver2 + znver3 + znver4 + lujiazui + nehalem + westmere
             + sandybridge + ivybridge + haswell + broadwell + skylake
             + skylake_avx512 + cascadelake + cooperlake + cannonlake
             + icelake_client + icelake_server + tigerlake + rocketlake
             + alderlake + sapphirerapids + graniterapids + graniterapids_d;
    if (subs > 1) bad++;
    return bad ? 9 : 0;
}
