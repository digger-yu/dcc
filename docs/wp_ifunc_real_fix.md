# WP：把 IFUNC resolver 从内置链接器搬回 codegen —— 真修报告

任务：`docs/ssa-pass-upgrade-plan.md` §7.38 / §7.41 三、`docs/missing_features.md` 的 IFUNC 行
所记的「IFUNC 使链接静默从动态降级为静态」。本报告只写**实测**与**代码位置**；
上一轮（底线那轮）的边界说明在 `docs/wp_ifunc.md`，本轮不再重复论证已实定的事实。

本报告是**全量重写**的第二版。第一版记的是「366 字节 init / 5 次 cpuid / 17 槽 68 字节缓存 /
49 个 supports 名字 / `__builtin_cpu_is` 只有厂商名」那一版实现；本轮在此之上加了
**型号分类链**（family/model 解码 + gcc 型号表）、**子叶探测**（(7,1):EAX、(0xD,1):EAX、
(0x8000_0008):EBX）与 **51 个 `__builtin_cpu_is` 型号名**，所以那些偏移/尺寸/计数全部作废，
本文里的每一个读数都是**本轮最后一次源码改动之后**重编译的二进制重跑的（见下）。

复现命令（本机 = x86-64 Intel family 6 model 167，支持 sse4.2/avx/avx2，**不**支持
avx512f/movdiri/clwb/serialize）：

```sh
D=/root/dcc/.work_tmp/tgt_ifunc/release/dcc      # cargo build --release -j1（11:52 完成，零 warning）
$D -O0 -c tests/fixtures/c/attr_target_clones.c -o tc.o
LC_ALL=C readelf -sW tc.o | grep -E 'IFUNC|<OS spec|resolver|__dcc_cpu'
LC_ALL=C readelf -rW tc.o | awk '/\.rela\.text/{f=1;next} /^Relocation section/{f=0} f'
objdump -d -j .text --start-address=0x1160 --stop-address=0x11eb tc.o
objdump -d -j .text --start-address=0x350  --stop-address=0x1160 tc.o      # init 体
$D -O0 tests/fixtures/c/attr_target_clones.c -o tc_dcc    # → stderr 两行，末行 `... (via host cc)`
file tc_dcc; LC_ALL=C readelf -dW tc_dcc | grep NEEDED; LC_ALL=C readelf -lW tc_dcc | grep -i interp
LC_ALL=C readelf -rW tc_dcc | grep IRELATIVE; LC_ALL=C readelf -sW tc_dcc | grep -E 'IFUNC|resolver'
./tc_dcc | diff - <(gcc -O0 tests/fixtures/c/attr_target_clones.c -o tc_gcc && ./tc_gcc)
$D -O0 tests/fixtures/c/attr_target_clones_selected.c -o sel_dcc; ./sel_dcc
$D -O0 -fbuiltin-linker /tmp/pure.c -o pure_static        # 纯 clones，无 libc
```

**为什么命令里都要 `LC_ALL=C`**：本机 zh_CN.UTF-8 下 readelf 把 `STT_GNU_IFUNC` 打成
`<操作系统专用>：10`，于是任何 `grep IFUNC` 都会假阴性。根因见 §1.1 末尾（是 ELF 头的
`EI_OSABI`，不是 locale —— 两者叠在一起）。交叉核对建议同时用 `objdump -t`，它打单字符 `i`
且不受 locale 影响；本报告两处都给了。

**本轮末的复核**（同一个二进制，全部重跑）：

* `.o`：`__dcc_cpu_model` 是 **100 字节** LOCAL OBJECT 坐 `.data`（PROGBITS），
  `__dcc_cpu_indicator_init` 是 **3590 字节** LOCAL FUNC @0x350，
  `compute`/`multiply` 是 `<OS specific>: 10`（= `STT_GNU_IFUNC`）@0x1160/41、@0x1190/91，
  与 `compute.resolver`/`multiply.resolver`（FUNC WEAK）**同址同尺寸**；
  全 `.o` `cpuid` **8** 次、`xgetbv` **1** 次；`.text` size `0x11eb`。
* exe：`file` 报 `dynamically linked, interpreter /lib64/ld-linux-x86-64.so.2`；
  `readelf -dW` 有 `(NEEDED) [libc.so.6]`；`readelf -lW` 有 `INTERP` 段；
  `readelf -rW` 两条 `R_X86_64_IRELATIVE`（`4053b8→4032b0`、`4053a8→4032e0`）；
  exe 的 symtab 里 `compute` 直接打成 `IFUNC`（exe 的 `EI_OSABI` 已由 `ld` 设成 GNU）。
* 逐形对照：7 个 target_clones 夹具 × `-O0/-O1/-O2/-O3` = 28 组，**24 组 stdout 与 rc 与 gcc
  逐字相同**，4 组是 `attr_target_clones_selected.c` 的**有意**差异（§2.4 末尾，两处原因）。
* builtin：`__builtin_cpu_supports` 的 **60** 名字矩阵与 gcc 逐字相同（`sum=45`）；
  `__builtin_cpu_is` 的 **51** 型号名矩阵与 gcc 逐字相同（本机只有 `intel=1`、`corei7=1`、
  `rocketlake=1` 三行为真、`sum=3`）；`.o` 字节级判据实测 init 体写进 `SLOT_CPU_TYPE` 的 id 集是 **17** 个、
  写进 `SLOT_CPU_SUBTYPE` 的是 **31** 个、写进 `SLOT_VENDOR_KIND` 的是 `{1,2,3,4}`
  —— 与 `cpu_isa::CLASSIFY` 表逐项相等（§1.6）。
* 静态路径：`-fbuiltin-linker` 三行播报齐全、rc=0、`compute`@0x4001c8 == `compute.default`、
  `__dcc_cpu_model` 100 字节 @0x403000 落在 `GNU_RELRO` 的 `FileSiz=0x64` 里（§4）。
* 测试：`--lib link::` **268** passed（0 failed）+ `semantics::cpu_isa` **26** 条 = 本包相关的
  lib 侧全量 **294**；七个相关 filter（各自计数与重叠说明见 §8.1）：`--lib cpu_isa` **28**、
  `--lib link::target_clones` **18**、`--lib link::ifunc` **9**、
  `--lib codegen::x86_64::ifunc` **7**、`integration target_clones` **16**、`integration cpu` **25**、
  `integration ifunc` **5**，全部 0 failed（§8）。

---

## 0. 终态 1-5 的达成情况（逐条 + 证据指针）

| # | 要求 | 状态 | 直接证据 |
|---|------|------|----------|
| 1 | `.o` 里 IFUNC 的 `st_value` 指向**真 resolver 体**：按变体优先级做 CPUID、返回选中变体地址；resolver 是普通 `STT_FUNC` | **做到** | §1.1/§1.2：`compute` type=10 @0x1160/41 == `compute.resolver` `FUNC WEAK` @0x1160/41，且与每个实现体（0x0/0x68/…）**都不同址**；体是 `e8 call __dcc_cpu_indicator_init` + `f7 05 testl $0x100000,__dcc_cpu_model+…(%rip)` +（AVX 那一支再加 `8b 05`+`and $0x6`+`cmp $0x6`）+ `48 8d 05 lea <variant>` + `ret`，五个变体地址由 `.rela.text` 的 `R_X86_64_PC32` 给出 |
| 2 | 「含 IFUNC 就改道内置链接器」不再需要：宿主 `cc`/`ld` 原生处理该 `.o`，产**动态**可执行，运行与 gcc 一致，且真选中 avx/sse4.2 可观察 | **做到** | §2：`file` 报 `dynamically linked`、`INTERP` 段 + `(NEEDED) [libc.so.6]`、两条 `R_X86_64_IRELATIVE`、exe 里 `compute` 是 `IFUNC` 且指向 `compute.resolver`；stderr `dcc: tc3 (via host cc)`；`diff` 与 gcc stdout 逐字相同（`compute(10) = 21` / `multiply(3, 4) = 12`），四档 28 组中 **24 组逐字相同**（剩下 4 组就是这一条夹具的有意差异，§2.6）；§2.4 的两支地址差 `delta=-448`（= `compute.sse4_2 - marker`，绑 default 会是 -344）与 `delta3=-240`（= `dispatch3.avx - marker`，sse4_2 是 -160、default 是 -80）——**avx 那一支也真被选中并可观察**；§2.5 另有行为级证据（两个实现体返回值不同），§2.7 证明 `gcc tc3.o` 直接链通 |
| 3 | `__builtin_cpu_supports` / `__builtin_cpu_is` 真实现（运行时 CPUID + 缓存 + 自包含） | **做到** | §3：两者都转发到与 resolver **同一条** `emit_feature_check`/分类链；`.o` 里 3590 字节的 `__dcc_cpu_indicator_init`（8 条 `cpuid` + 1 条 `xgetbv` + `SLOT_INIT_DONE` 幂等守卫）与 100 字节 `.data` 缓存，对 libgcc 的 `__cpu_model`/`__cpu_indicator_init` UND 引用数 **0**（gcc 的 `.o` 是 2）；60 名字 supports 矩阵 + 51 名字 cpu_is 矩阵与 gcc 逐字相同；§3.3 记的**四个真位号/门控错**（`rdseed`、扩展叶根门控、`movdiri`、`serialize`）与 §1.6 的字节级 id 覆盖判据都是这一轮新增的 |
| 4 | 三态判据翻成断言动态 | **做到** | §5：`target_clones_diverts_to_static_link_is_known_defect` → **`target_clones_links_dynamically_with_codegen_resolver`**，断 `file` 报 dynamically linked + `NEEDED libc.so.6` + 两条 IRELATIVE + IFUNC 与自己的 resolver 同址同尺寸 + `via host cc` + 与 gcc 逐字 stdout；判据注释里的实测地址已按本轮重采（旧 `404318/402430` → `405318/4030c0`；该 exe 与本报告 §2.1 的 `tc3` 因构建方式不同而地址漂，不变量是「IRELATIVE 存在 + IFUNC 与自己的 resolver 同址同尺寸」，见 §5） |
| 5 | 底线不退：无 IFUNC 仍动态；仍走内置链接器的路径继续明说链接方式 | **做到** | §4：基准线（无 IFUNC）在同一次运行里断 `dynamically linked`（判据 `target_clones_links_dynamically_with_codegen_resolver` 的第 1 步）；`-fbuiltin-linker` 的 stderr 三段齐全（降级原因 + 绑到哪个变体 + `static (no PT_INTERP / DT_NEEDED)`），且 `target_clones_link_mode_is_announced_on_both_paths` 同时钉两条路径的措辞 |

---

## 1. `.o` 侧实测

### 1.1 符号表（`tests/fixtures/c/attr_target_clones.c`，`dcc -O0 -c`）

```
$ LC_ALL=C readelf -sW tc.o | grep -E 'IFUNC|<OS spec|resolver|__dcc_cpu'
     1: 0000000000000000   100 OBJECT  LOCAL  DEFAULT    3 __dcc_cpu_model
     7: 0000000000000350  3590 FUNC    LOCAL  DEFAULT    1 __dcc_cpu_indicator_init
   103: 0000000000001160    41 <OS specific>: 10 GLOBAL DEFAULT    1 compute
   104: 0000000000001190    91 <OS specific>: 10 GLOBAL DEFAULT    1 multiply
   105: 0000000000001160    41 FUNC    WEAK   DEFAULT    1 compute.resolver
   106: 0000000000001190    91 FUNC    WEAK   DEFAULT    1 multiply.resolver

$ objdump -t tc.o | grep -E 'compute|__dcc_cpu'      # 不受 locale 影响的交叉核对
0000000000000000 l     O .data	0000000000000064 __dcc_cpu_model
0000000000000000 l     F .text	0000000000000064 compute.sse4_2
0000000000000068 l     F .text	0000000000000064 compute.default
00000000000000d0 l     F .text	0000000000000052 multiply.avx
0000000000000128 l     F .text	0000000000000052 multiply.sse4_2
0000000000000180 l     F .text	0000000000000052 multiply.default
0000000000000350 l     F .text	0000000000000e06 __dcc_cpu_indicator_init
0000000000001160 g   i   .text	0000000000000029 compute
0000000000001190 g   i   .text	000000000000005b multiply
0000000000001160  w    F .text	0000000000000029 compute.resolver
0000000000001190  w    F .text	000000000000005b multiply.resolver
```

`0xe06` = 3590、`0x64` = 100；`objdump -t` 的类型列 `i` 就是 `STT_GNU_IFUNC`。
原始字节的交叉核对（`st_info`，两个编译器逐个比过）：

| 符号 | dcc `.o` | gcc `.o` |
|------|----------|----------|
| `compute` | `st_info=0x1a`（type=10 bind=GLOBAL）、`val=0x1160 size=41`、`shndx=1` | `st_info=0x1a`、`val=0x0 size=50`、`shndx=10` |
| `compute.resolver` | `st_info=0x22`（type=FUNC bind=WEAK）、`val=0x1160 size=41` | `st_info=0x22`、`val=0x0 size=50` |

要点：

* **`st_value`/`st_size` 与 `<base>.resolver` 逐字节相等**（0x1160/41、0x1190/91），且与任何
  实现体地址都**不相等**。任务书里那条已实定的旧缺陷形状（「IFUNC 与实现体同址、全 `.o`
  无 resolver 体」）由判据 `attr_target_clones_ifunc_symbols` 反向钉死：它遍历
  `compute`/`multiply` 与每个 `compute.*`/`multiply.*`，断言 `assert_ne!(base.value, variant.value)`，
  旧形状一出现就红。
* gcc 的 `st_value` 是 0 而 dcc 的是节内偏移，只是「gcc 把每个 resolver 放进独立
  `.text.compute.resolver` 节（section-GC 友好）、dcc 全放一个 `.text`」的差别；
  **符号类型与绑定完全一致**：基名 `GLOBAL + IFUNC`、resolver `WEAK + FUNC`。
* `<base>.resolver` 是**普通 `STT_FUNC`**，IFUNC 符号指向它 —— 正是要求的 gcc 形状。
* **`<OS specific>` 与 locale 两件事叠加**（这是本轮采数时踩到的，值得记下来）：
  readelf 只在 ELF 头的 `EI_OSABI == ELFOSABI_GNU(3)` 时把 `st_type==10` 打成 `IFUNC`，
  否则落进 `STT_LOOS..STT_HIOS` 打成 `<OS specific>: 10`（中文 locale 再译成
  `<操作系统专用>：10`）。实测：dcc 的 `.o` 是 `OS/ABI: UNIX - System V`（`EI_OSABI=0`），
  gcc 的 `.o` 是 `UNIX - GNU`（3）。把 dcc 的 `.o` 第 7 字节手工改成 3 再读，
  同一份文件立刻打 `IFUNC`，而且宿主 `ld` 照样链通（`gcc tc_osabi3.o -o o3exe` → rc=0，
  exe 的 OS/ABI 变 GNU）。所以：
  * **语义上没问题**（`ld` 认 0x1a，IRELATIVE 正常生成，见 §2）；
  * 但肉眼判读会误判，所以本报告的命令一律用数值判据（`st_info & 0xf == 10`）而不是 grep 文本；
  * `EI_OSABI` 由 `src/codegen/shared/elf.rs:374`/`:1484`/`:2520`/`:2743`/`:2788`
    五处 `out.push(0); // ei_osabi` 写死，**该文件不在本包所有权内**（`codegen/shared`），
    列为跨所有权建议（§7.2 第 1 条）。

### 1.2 resolver 体的机器码（`objdump -d`，**不是** `dcc -S` 的自家反汇编）

```
0000000000001160 <compute.resolver>:              ; = IFUNC 'compute' 的 st_value
    1160:	e8 00 00 00 00       	call   1165        ; __dcc_cpu_indicator_init
    1165:	f7 05 00 00 00 00 00 	testl  $0x100000,0x0(%rip)   ; leaf1/ECX#20 = SSE4.2
    116c:	00 10 00
    116f:	0f 84 0c 00 00 00    	je     1181
    1175:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        ; compute.sse4_2
    117c:	e9 07 00 00 00       	jmp    1188
    1181:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        ; compute.default
    1188:	c3                   	ret

0000000000001190 <multiply.resolver>:             ; 三档链 avx → sse4.2 → default
    1190:	e8 00 00 00 00       	call   1195        ; __dcc_cpu_indicator_init
    1195:	f7 05 00 00 00 00 00 	testl  $0x10000000,0x0(%rip) ; leaf1/ECX#28 = AVX
    119c:	00 00 10
    119f:	0f 84 22 00 00 00    	je     11c7
    11a5:	8b 05 00 00 00 00    	mov    0x0(%rip),%eax        ; XCR0 缓存
    11ab:	25 06 00 00 00       	and    $0x6,%eax             ; SSE+YMM 状态位
    11b0:	3d 06 00 00 00       	cmp    $0x6,%eax
    11b5:	0f 85 0c 00 00 00    	jne    11c7
    11bb:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        ; multiply.avx
    11c2:	e9 23 00 00 00       	jmp    11ea
    11c7:	f7 05 00 00 00 00 00 	testl  $0x100000,0x0(%rip)   ; SSE4.2
    11ce:	00 10 00
    11d1:	0f 84 0c 00 00 00    	je     11e3
    11d7:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        ; multiply.sse4_2
    11de:	e9 07 00 00 00       	jmp    11ea
    11e3:	48 8d 05 00 00 00 00 	lea    0x0(%rip),%rax        ; multiply.default
    11ea:	c3                   	ret
```

`0x0(%rip)` 的空槽由重定位填。`readelf -rW tc.o` 的 `.rela.text` 里落在两个 resolver 范围内的
**全部**条目（逐字）：

```
0000000000001161  R_X86_64_PC32  __dcc_cpu_indicator_init - 4
0000000000001167  R_X86_64_PC32  __dcc_cpu_model + 10          ; SSE4.2 槽
0000000000001178  R_X86_64_PC32  compute.sse4_2 - 4
0000000000001184  R_X86_64_PC32  compute.default - 4
0000000000001191  R_X86_64_PC32  __dcc_cpu_indicator_init - 4
0000000000001197  R_X86_64_PC32  __dcc_cpu_model + 10          ; AVX（同一槽，不同掩码）
00000000000011a7  R_X86_64_PC32  __dcc_cpu_model + 34          ; XCR0 槽
00000000000011be  R_X86_64_PC32  multiply.avx - 4
00000000000011c9  R_X86_64_PC32  __dcc_cpu_model + 10          ; SSE4.2（multiply 的第二档）
00000000000011da  R_X86_64_PC32  multiply.sse4_2 - 4
00000000000011e6  R_X86_64_PC32  multiply.default - 4
```

即：**入门先调检测函数**（不依赖任何人已经跑过 init）、**判定读缓存**、**返回值是被选中变体的地址**。
`compute` 的 `testl $0x100000` = leaf1/ECX bit20 = SSE4.2，掩码由 `cpu_isa` 的表算出，
codegen 里没有写死任何 leaf/位号（§6）。

**加数与槽号的换算（最容易看错的一处，本轮真的看错过一次）**：readelf 打印的是
`st_value + r_addend`，`__dcc_cpu_model` 的 `st_value = 0`，所以显示值 = `字段偏移 - (L - f)`，
`L` 是整条指令长度、`f` 是 disp32 相对指令起始的偏移（每个发射形式自带，
`src/codegen/x86_64/ifunc.rs:1461-1507` 的 `RipForm::delta`）。**两个坑**：

1. `readelf` 的 addend 是**十六进制**。把 `+ 10` 当十进制 → 字段偏移 14 → `/4` 得 3.5，
   看起来像「槽号不整齐」；按十六进制 `0x10` → 字段偏移 `0x18` → 槽 6，恰好是 `SLOT_LEAF1_ECX`。
2. `delta` 随发射形式变：`F7 05 disp32 imm32`（test）与 `C7 05 disp32 imm32`（movl）是 8，
   `8B 05`/`89 05`/…（mov）与 `48 8d 05`（lea）是 4。混用会把掩码对的位读到错的槽上 ——
   正是「CPUID 测了、位也对、但读错寄存器」那种只能靠数值换算钉住的错。

| 读法 | 指令 | `L`/`f` | delta | 显示加数 | 字段偏移 | 槽 |
|------|------|---------|-------|----------|----------|----|
| `testl $0x100000, …+10` | `F7 05 disp32 imm32` | 10 / 2 | 8 | `0x10` | `0x18` | 6 = `SLOT_LEAF1_ECX` ✅ |
| `mov …+34,%eax`（XCR0） | `8B 05 disp32` | 6 / 2 | 4 | `0x34` | `0x38` | 14 = `SLOT_XCR0_LOW` ✅ |
| `movl $imm, …+58`（分类链） | `C7 05 disp32 imm32` | 10 / 2 | 8 | `0x58` | `0x60` | 24 = `SLOT_CPU_SUBTYPE` ✅ |
| `call __dcc_cpu_indicator_init - 4` | `E8 disp32` | 5 / 1 | 4 | `-4` | 0 | 符号起始 ✅ |

这一层由判据 **`attr_target_clones_resolver_bits_come_from_cpu_isa_table`** 钉：它从 `.text`
的字节辨出发射形式（`F7 05`→8、`8B 05`→4），把 `r_addend` 还原成 `(槽, 位)`，再与
`cpu_isa::cpuid_bits(arch)` 经 `cache_slot(leaf, subleaf, reg)` 映射出的期望**逐条比对**；
按 `r_offset` 排序重走 resolver 时以变体 `lea` 闭合一段，所以还顺带钉了「每段读的位就是那个
变体的位」。另附三条防伪：`default` 段必须**零**检测、只有表里 `xcr0_mask != 0` 的基名才允许
出现 XCR0 读、每个基名必须真读到至少一条特性位（否则「空对空」也会算过）。

### 1.3 CPUID 检测函数体（`objdump -d`）

`__dcc_cpu_indicator_init` @0x350，**3590** 字节。幂等守卫 + 9 次探测 + 分类链：

```
 350:	53                    	push   %rbx
 351:	8b 05 00 00 00 00    	mov    0x0(%rip),%eax   ; SLOT_INIT_DONE
 357:	85 c0                	test   %eax,%eax
 359:	0f 85 .. .. .. ..    	jne    <done>          ; 已测过 → 直接返回（幂等）
```

探测序列（每条都由 `objdump` 上下文里的 `mov $imm,%eax` / `mov $imm,%ecx` 认出 leaf/subleaf，
前置的 `cmp`+`jb` 就是门控；逐条实测）：

| `.o` 地址 | 指令 | leaf/subleaf | 门控 | 写入的槽 |
|-----------|------|--------------|------|----------|
| `0x366` | `cpuid` | (0, 0) | 无（根） | `SLOT_LEAF0_EAX` + `SLOT_VENDOR_BX/CX/DX` |
| `0x398` | `cpuid` | (1, 0) | `cmp $0x1,%eax; jb` ← `SLOT_LEAF0_EAX` | `SLOT_LEAF1_EAX/EBX/ECX/EDX` |
| `0x3ca` | `cpuid` | (7, 0) | `cmp $0x7,%eax; jb` | `SLOT_LEAF7_EBX/ECX/EDX` |
| `0x3ff` | `cpuid` | (7, **1**) | `mov $0x7,%eax; mov $0x1,%ecx` + `cmp $0x1`（`SLOT_LEAF7_MAX`） | `SLOT_LEAF7S1_EAX` |
| `0x422` | `cpuid` | (0xD, **1**) | `mov $0xd,%eax; mov $0x1,%ecx` + `cmp $0xd` | `SLOT_LEAFD1_EAX` |
| `0x431` | `cpuid` | (0x80000000, 0) | 无（扩展叶根，**故意不带门控**，见下） | `SLOT_EXT_MAX_EAX` |
| `0x451` | `cpuid` | (0x80000001, 0) | `cmp $0x80000001,%eax; jb` ← `SLOT_EXT_MAX_EAX` | `SLOT_EXT1_ECX/EDX` |
| `0x477` | `cpuid` | (0x80000008, 0) | `cmp $0x80000008,%eax; jb` | `SLOT_EXT8_EBX` |
| `0x491` | `xgetbv` | `%ecx=0` | `testl $0x800000,` OSXSAVE（leaf1/ECX#27） | `SLOT_XCR0_LOW` |

全 `.o` 里 `cpuid` **8** 次、`xgetbv` **1** 次（任务书里那条已实定事实「全 `.o` 无 CPUID」的
反面）。0x431 那一条**必须**没有门控：上一轮它读的是**基本**叶最大值并要求 `>= 0x8000_0000`
（本机基本叶最大值 0x20）→ 永假 → 整个扩展叶区被跳过，`lzcnt`/`abm`/`lm`/`nx` 等**全体静默读 0**
而其它一切照常，是典型的「看起来修了」。防复发不靠再补名字断言，而靠结构不变量
**`probe_gates_are_satisfiable`**（`src/semantics.rs`）：门控读的槽必须由更早的探测写入、
`SLOT_LEAF0_EAX` 作门控槽时 `min < 0x8000_0000` 且该探测不得是扩展叶（`SLOT_EXT_MAX_EAX`
反之）、两条根探测必须 `Gate::None`。

函数尾部（`0x434` 起）是厂商识别与型号分类链，`0x350..0x1160` 一共 3590 字节里绝大部分就是它
（为什么这么长见 §1.6）。厂商识别按三个 dword 比：

```
 434:	3d 47 65 6e 75       	cmp    $0x756e6547,%eax   ; "Genu" → Intel 链
 476:	3d 41 75 74 68       	cmp    $0x68747541,%eax   ; "Auth" → AMD 链
```

### 1.4 CPUID 缓存对象的槽布局（**本轮从 17 槽扩到 25 槽**）

```
$ LC_ALL=C readelf -SW tc.o | grep -E '\.text |\.data|\.bss'
  [ 1] .text    PROGBITS  0000000000000000 000040 0011eb 00  AX  0  0 16
  [ 3] .data    PROGBITS  0000000000000000 001258 000064 00  WA  0  0  8
  [ 4] .bss     NOBITS    0000000000000000 0012c0 000000 00  WA  0  0  8
```

`__dcc_cpu_model`：`st_shndx = 3 = .data`（PROGBITS）、`size = 0x64 = 100 = cpu_isa::MODEL_BYTES`
（= `SLOT_WORDS(25) * 4`，`src/semantics.rs:1252`/`:1254`）。槽表（`src/semantics.rs:1214-1252`，
本文里所有 `semantics.rs` 行号都是本轮末次改动后的读数）：

| 槽 | 名字 | 槽 | 名字 | 槽 | 名字 |
|----|------|----|------|----|------|
| 0 | `LEAF0_EAX` | 8 | `LEAF7_EBX` | 16 | `INIT_DONE` |
| 1 | `VENDOR_BX` | 9 | `LEAF7_ECX` | 17 | `LEAF7_MAX` |
| 2 | `VENDOR_CX` | 10 | `LEAF7_EDX` | 18 | `LEAF7S1_EAX` |
| 3 | `VENDOR_DX` | 11 | `EXT_MAX_EAX` | 19 | `LEAFD1_EAX` |
| 4 | `LEAF1_EAX` | 12 | `EXT1_ECX` | 20 | `EXT8_EBX` |
| 5 | `LEAF1_EBX` | 13 | `EXT1_EDX` | 21 | `CPU_FAMILY` |
| 6 | `LEAF1_ECX` | 14 | `XCR0_LOW` | 22 | `CPU_MODEL` |
| 7 | `LEAF1_EDX` | 15 | `VENDOR_KIND` | 23 | `CPU_TYPE` |
|   |             |    |             | 24 | `CPU_SUBTYPE` |

本轮新增的 8 个槽（16..24 里除 16/17 外）全部是为了**分类链**与**子叶**：
`SLOT_LEAF7_MAX`（(7,0):EAX = leaf7 最大子叶）与 `SLOT_LEAFD1_EAX`/`SLOT_LEAF7S1_EAX`/
`SLOT_EXT8_EBX` 三个子叶结果各自独立成槽 —— 这是判据
`subleaf_slots_are_distinct_from_subleaf_zero` 与 `linker_path_cloneable_bits_have_no_subleaf`
要钉的东西：`(7,1):EAX` 绝不能与 `(7,0):EAX` 共用槽，而**内置链接器**那条路径消费的 cloneable
位集里没有任何子叶位（它只服务 `target_clones`，其可 clone 的名字全在 leaf1/leaf7 子叶 0/扩展叶
子叶 0），所以那条路径的缓存键 `(leaf, subleaf)` 在 subleaf≠0 时必须永不出现。

`.data` 而不是 `.bss` 不是风格选择：内置链接器的静态路径**不映射 NOBITS 输出节**
（`src/link/layout.rs:673` 的 `fill_section_data` 跳过 NOBITS、
`src/link/elf_exec.rs:104`/`:186`/`:220` 的 `segment_range_actual` 同样跳过），
实测反面基准 `static int buf[8];` 走 `-fbuiltin-linker` 得 **rc=139**。缓存若坐 `.bss`，
`__dcc_cpu_indicator_init` 的第一个写就 SIGSEGV。`elf64_section_names()` +
`attr_target_clones_ifunc_symbols` 把这条钉成断言：改回 `.bss` 立刻红。§4 里静态产物的
`GNU_RELRO FileSiz=0x64` 是同一件事的正面的证据。

### 1.5 判定链按 ISA 优先级，而不是按书写顺序

夹具 `attr_target_clones_select.c` 故意打乱书写顺序（`pick` 写
`"sse4.2,avx512f,avx,default"`、`pick_static` 写 `"default,avx,sse4.2"`）。
`dcc -O0 -c` 后按 `r_offset` 排序的实测链（本轮重采）：

```
00000000000013ee  pick.avx512f - 4
0000000000001420  pick.avx - 4
000000000000143c  pick.sse4_2 - 4
0000000000001448  pick.default - 4
000000000000147e  pick_static.avx - 4
000000000000149a  pick_static.sse4_2 - 4
00000000000014a6  pick_static.default - 4
```

书写序是 `sse4.2,avx512f,avx,default`，链序是 `avx51f → avx → sse4_2 → default` ——
**由 `cpu_isa::dispatch_priority` 决定，与书写无关**；第二支同理把 `default` 从链首挪到链尾。
判据 `attr_target_clones_select_resolver_dispatch_order` 写死这两段期望序列，任何一处回到
「声明序」或「只认 default」都立刻红，且与本机 CPU 无关。
`pick_static` 是 `static` 函数，它的 IFUNC 条目也必须是 `LOCAL`（实测
`11: 0x1450 91 <OS specific>: 10 LOCAL pick_static`，与 gcc 同形）。

### 1.6 分类链的字节级覆盖判据（本轮新增，`__builtin_cpu_is` 的型号名靠它）

3590 字节的 init 体里，绝大部分是「family/model 解码 + gcc 型号分类链」：它把 leaf1 EAX 解成
`family`/`model`（`decode_family_model`，`src/semantics.rs:1945`），再按厂商分支逐条比较
`model` 与特性守卫，命中的 `cpu_type`/`cpu_subtype` **id** 写进槽 23/24。id 由
`type_id`/`subtype_id`（`:1995`/`:2020`）从 `CLASSIFY` 表（`:1735`）算出，`__builtin_cpu_is("haswell")`
就是 `cmpl $id, SLOT_CPU_SUBTYPE(%rip)`。

问题：怎么证明「表里 17 个类型 + 31 个子型」**全都**发进机器码了？只与 gcc 对照是不够的
（本机是 Intel，AMD/Zhaoxin 支路永不命中，缺一条也测不出）。本轮的判据
**`attr_target_clones_cpu_indicator_classify_ids_come_from_classify_table`**
（`tests/integration/c_attr_builtin.rs`）**不运行任何东西**，而是读 `.o` 字节：

* 扫 `.text` 里每条 `C7 05`（`movl $imm32, disp32(%rip)`）+ 它的 `R_X86_64_PC32` 加数，
  按 §1.2 的换算（delta=8）还原成 `(槽, 立即数)`；
* 要求槽 23 上观测到的 id 集合与 `CLASSIFY` 里推出的 `type_id` 集合**相等**，槽 24 同理；
* 并要求槽 15（`VENDOR_KIND`）覆盖四条 `VENDOR_SIGS` 的 kind。

本机实测（`.work_tmp/meas2/an.py`，只读分析脚本，不进仓库）：

```
指向 __dcc_cpu_model 的 C7 05（movl $imm, disp32(%rip)）写入：
  slot 15 (offset  60): 4 个不同值 contiguous_from_1=True -> ['0x1','0x2','0x3','0x4']
  slot 16 (offset  64): 1 个不同值 -> ['0x1']                       ; INIT_DONE 置 1
  slot 23 (offset  92): 17 个不同值 -> ['0x1'..'0x8','0x19','0x1c'..'0x23']
  slot 24 (offset  96): 31 个不同值 -> ['0x1'..'0x18','0x1a','0x1b','0x20','0x21','0x24','0x25','0x29']
```

17 / 31 与 `CLASSIFY` 表里的类型名/子型名个数逐项相等（判据就是断相等，不是断包含）。
`slot 16 -> {1}` 也顺带证明幂等守卫真的写 1。

运行时侧的答案由另一条判据钉，且**期望不写死本机**：
`attr_target_clones_cpu_is_model_names_match_gcc` 做两层 ——
① 51 个型号的 `__builtin_cpu_is` 在四档下与 `gcc -O0` 逐字对照；
② 同一批读数逐个与参考求值器 `cpu_isa::cpu_is_holds(name, kind, &class)` 对账，
其中 `(kind, class)` 来自 `cpu_isa::host_processor_class()`（Rust 侧自己读 `/proc`+CPUID 的
同一份表）。于是「本机答案是什么」不是判据的前提，但「实现与表是否一致」是。

本机（Intel family 6 model 167）实测 51 行输出：

```
$ ./ci_dcc | diff - <(./ci_gcc) && echo IDENTICAL
IDENTICAL                        ; 52 行（51 名字 + sum）逐字相同
$ grep -n '=1' ci_dcc            ; 每个名字一行，只有三行命中
1:intel=1
10:corei7=1
47:rocketlake=1
$ grep '^sum=' ci_dcc
sum=3
```

`rocketlake` 这一条是分类链真的跑通的直接证据（它要求 Intel + family 6 + model ∈ 0x8d/0xb5/0xb6…
的 `MPred` 命中），而 `corei7` 是**组级类型**（`group_type`）被写进槽 23 的证据。

---

## 2. 可执行文件侧实测（宿主 `cc`/`ld` 原生吃下这个 `.o`）

夹具 `tests/fixtures/c/attr_target_clones.c`（`compute` = `sse4.2,default`、
`multiply` = `avx,sse4.2,default`，都用 `printf`）。命令是
`dcc -O0 tests/fixtures/c/attr_target_clones.c -o tc3`。

### 2.1 链接方式与 IFUNC 语义

```
$ file tc3
tc3: ELF 64-bit LSB executable, x86-64, version 1 (GNU/Linux), dynamically linked,
     interpreter /lib64/ld-linux-x86-64.so.2, BuildID[sha1]=9ef9d3…, for GNU/Linux 3.2.0, not stripped

$ cat tc3.err
dcc: wrote tc3.o
dcc: wrote /tmp/dcc-hostlink-1065420/dcc-hostlink-attr_target_clones.c.o
dcc: .work_tmp/meas3/tc3 (via host cc)          ; ← 路径自己说出来（底线：不得静默）

$ LC_ALL=C readelf -dW tc3 | head -6
Dynamic section at offset 0x41b0 contains 24 entries:
 0x0000000000000001 (NEEDED)    Shared library: [libc.so.6]
 0x000000000000000c (INIT)       0x402000
 0x0000000000000019 (INIT_ARRAY) 0x4051a0

$ LC_ALL=C readelf -lW tc3 | grep -A1 INTERP
  INTERP  0x0002e0 0x…4002e0 0x…4002e0 0x0001c 0x0001c R 0x1
      [Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]

$ LC_ALL=C readelf -rW tc3 | grep -i irelative
00000000004053b8  0000000000000025 R_X86_64_IRELATIVE   4032b0
00000000004053a8  0000000000000025 R_X86_64_IRELATIVE   4032e0

$ LC_ALL=C readelf -sW tc3 | grep -E 'IFUNC|resolver|__dcc_cpu'
    12: 00000000004053d0  100 OBJECT LOCAL DEFAULT 24 __dcc_cpu_model
    18: 00000000004024a0 3590 FUNC   LOCAL DEFAULT 14 __dcc_cpu_indicator_init
    39: 00000000004032e0   91 IFUNC  GLOBAL DEFAULT 14 multiply
    72: 00000000004032e0   91 FUNC   WEAK  DEFAULT 14 multiply.resolver
   111: 00000000004032b0   41 FUNC   WEAK  DEFAULT 14 compute.resolver
   127: 00000000004032b0   41 IFUNC  GLOBAL DEFAULT 14 compute
```

四条要求逐条对上：

* `PT_INTERP` + `NEEDED libc.so.6` + `file` 报 `dynamically linked` ⇒ **不是**静态产物；
* `R_X86_64_IRELATIVE`(0x25) 两条，`offset` 是 GOT 槽、`sym` 列空、尾部值是 **resolver 的地址**
  （`4032b0`/`4032e0`），与 exe symtab 里两个 `<base>.resolver` 完全一致 ⇒ 动态链接器会
  在重定位阶段**调用**它们；
* exe 里 `compute` 的 `st_value`/`st_size` 与 `compute.resolver` 逐字节相等，且与
  `compute.sse4_2`/`compute.default` 都不同址 ⇒ IFUNC 语义活到了最终产物，不是只在 `.o` 里；
* exe 的 symtab 里 `compute` 直接打成 `IFUNC` —— 因为 `ld` 把输出的 `EI_OSABI` 设成了
  GNU(3)，`readelf` 于是认得 10（`.o` 那侧为何打 `<OS specific>` 见 §1.1 末尾）。

顺带：`__dcc_cpu_model` 在 exe 里落到 **shndx 24**（`.data.rel.ro`），`__dcc_cpu_indicator_init`
仍在 `.text`(14)。也就是说 `.data`→`.data.rel.ro` 的搬移是 `ld` 做的，语义不变
（写时可读、RELRO 在初始化后才只读，而 init 体只被 CPUID 缓存的 `INIT_DONE` 守卫保护）。

### 2.2 resolver 体在最终镜像里的样子（`objdump -d`，重定位已解析）

```
00000000004032b0 <compute.resolver>:              ; = IFUNC 'compute' 的 st_value
  4032b0:	e8 eb f1 ff ff       	call   4024a0 <__dcc_cpu_indicator_init>
  4032b5:	f7 05 29 21 00 00 00 	testl  $0x100000,0x2129(%rip)   # 4053e8 <__dcc_cpu_model+0x18>
  4032bc:	00 10 00
  4032bf:	0f 84 0c 00 00 00    	je     4032d1 <compute.resolver+0x21>
  4032c5:	48 8d 05 84 ee ff ff 	lea    -0x117c(%rip),%rax       # 402150 <compute.sse4_2>
  4032cc:	e9 07 00 00 00       	jmp    4032d8 <compute.resolver+0x28>
  4032d1:	48 8d 05 e0 ee ff ff 	lea    -0x1120(%rip),%rax       # 4021b8 <compute.default>
  4032d8:	c3                   	ret
```

这一份读数是「resolver 真在镜像里、真做 CPUID、真返回变体地址」的最直接证据：
`call` 的目标已解析成 `__dcc_cpu_indicator_init` 的镜像地址；`testl` 的内存操作数已解析成
`__dcc_cpu_model + 0x18`，`0x18 / 4 = 6` = `SLOT_LEAF1_ECX`，立即数 `0x100000` = `1 << 20`
= SSE4.2（与 `cpu_isa::FEATURES` 一致，见 §1.2 的换算表）；两个 `lea` 分别解析成
`compute.sse4_2`(0x402150) 与 `compute.default`(0x4021b8)。

### 2.3 运行输出与 gcc 逐字相同

```
$ ./tc3
compute(10) = 21
multiply(3, 4) = 12
$ diff <(./tc3) <(./tc3.gcc) && echo STDOUT_IDENTICAL
STDOUT_IDENTICAL          ; rc 两侧都 0
```

（`tc3.gcc` = `gcc -O0 同夹具`。gcc 的产物是 PIE、dcc 的默认是非 PIE，所以这里只比
stdout/rc —— 这也正是判据打印**差值**而不是绝对地址的原因，见 §2.4。）

### 2.4 「真选中了非 default 变体」是可观察的

夹具 `attr_target_clones_selected.c`：两支 clones 函数（`compute` = `sse4.2,default`、
`dispatch3` = `avx,sse4.2,default`）+ 一个普通函数 `marker`，打印
`(void *)fn - (void *)marker` 与 `__builtin_cpu_supports` 的读数。

```
$ ./sel.d                                        ; dcc 产物
delta=-448  has_sse42=1 call=21
delta3=-240 has_avx=1  call3=17

$ LC_ALL=C readelf -sW sel.d | grep -E 'compute|dispatch3|marker'
    13: 0000000000402150 100 FUNC LOCAL DEFAULT 14 compute.sse4_2
    14: 00000000004021b8 100 FUNC LOCAL DEFAULT 14 compute.default
    15: 0000000000402220  80 FUNC LOCAL DEFAULT 14 dispatch3.avx
    16: 0000000000402270  80 FUNC LOCAL DEFAULT 14 dispatch3.sse4_2
    17: 00000000004022c0  80 FUNC LOCAL DEFAULT 14 dispatch3.default
   134: 0000000000402310  60 FUNC GLOBAL DEFAULT 14 marker
   127: 0000000000403330  41 IFUNC GLOBAL DEFAULT 14 compute      (== 111 compute.resolver)
   100: 0000000000403360  91 IFUNC GLOBAL DEFAULT 14 dispatch3    (== 52 dispatch3.resolver)
$ LC_ALL=C readelf -rW sel.d | grep -i irelative
00000000004053f0  0000000000000025 R_X86_64_IRELATIVE  403330
00000000004053e8  0000000000000025 R_X86_64_IRELATIVE  403360
```

把读数与符号表做减法是**闭合**的（`marker = 0x402310`）：

| 打印 | 值 | 等于哪个符号 | 若是「恒返 default」会是 |
|------|-----|--------------|--------------------------|
| `delta` | -448 | `compute.sse4_2`(0x402150) − marker | -344（`compute.default` 0x4021b8） |
| `delta3` | -240 | `dispatch3.avx`(0x402220) − marker | -80（default）、-160（sse4_2） |

`has_sse42=1`/`has_avx=1` 与「地址落在对应变体」是**两条独立通道**（前者走
`emit_cpu_supports`→`emit_feature_check`，后者走 resolver 体里的同一份 `emit_feature_check`
+ `lea`），两者互相印证，所以恒假桩、无条件返 default、无条件返第一个变体三种偷工做法
都会在这一条上红 —— 而 `call=21`/`call3=17` 证明绑定的地址确实**可调用且语义正确**。
第二支额外钉住优先级（avx 与 sse4.2 同时为真时走 avx）与 AVX 的 XCR0/OSXSAVE 门控。

与 gcc 的两处**有意**差异（因此这一条不做逐字对照，理由写在夹具头注释里）：

1. gcc 13 把 `(void *)ifunc` 绑到 **PLT 桩**：实测 `gcc -O0 -no-pie` 打 `delta=-4606`，
   配合 `sel.g` 的 `marker = 0x40227e` ⇒ `p = 0x40227e − 4606 = 0x401080`，
   落在 `.plt.sec` `[0x401060, 0x401090)`（`.plt` 起于 0x401020，`.text` 起于 0x401090）。
   调用语义相同，但只有 dcc 这一侧能直接读出选中了哪个变体；
2. `__builtin_cpu_supports` 的返回值：gcc 是 libgcc `__cpu_model` 私有枚举的位值
   （本机实测 `has_sse42=256`、`has_avx=512`，即 `1 << index`，随 gcc 版本变），
   dcc 归一成 0/1。C/gcc 只承诺「非零 = 支持」，逐字对齐那个位值等于复制外部实现的
   私有 ABI，所以不对齐；判据一律用 `!= 0` 取定，对这一条免疫。

### 2.5 行为级证据（`__attribute__((ifunc))` + 返回值不同的两个实现体）

dcc 目前**没有** per-variant 的 ISA codegen（`src/ast.rs` 里没有 `target_attrs`），
实测 `compute.sse4_2` 与 `compute.default` 的 `.text` 字节切片**逐字节相同**
（各 100 字节含填充；`multiply` 三个变体各 82 字节两两相同 —— 只靠地址差才能看出选型）。
所以「选型真发生」还需要一条行为级判据：
`tests/fixtures/c/attr_target_clones_ifunc_behavior.c` 用两个**返回值不同**的实现体
（`fast_impl`→2 / `slow_impl`→1）配手写 resolver。

```
$ ./beh5                                    ; dcc 产物
by_sse2=2 want_a=2
by_avx2=11 want_b=11        rc=0
$ ./beh5.gcc                ; 同一夹具交给 gcc
by_sse2=2 want_a=2
by_avx2=11 want_b=11        rc=0        → diff 无输出

$ LC_ALL=C readelf -sW beh5 | grep -E 'IFUNC|resolve'
   113: 0000000000402250 122 IFUNC GLOBAL DEFAULT 14 by_sse2
   133: 00000000004022d0 160 IFUNC GLOBAL DEFAULT 14 by_avx2
    17: 0000000000402250 122 FUNC  LOCAL  DEFAULT 14 resolve_sse2
    18: 00000000004022d0 160 FUNC  LOCAL  DEFAULT 14 resolve_avx2
$ LC_ALL=C readelf -rW beh5 | grep -i irelative
00000000004053b8  0000000000000025 R_X86_64_IRELATIVE  402250
00000000004053c0  0000000000000025 R_X86_64_IRELATIVE  4022d0
```

`by_sse2` 得到 **2**（`fast_impl`）而不是 1，是「CPUID 真跑过、选型真发生」的行为级
证据；x86-64 上 SSE2 恒支持（leaf1/EDX#26），所以「非 default 实现必被选中」在**任何**
x86-64 机器上都成立（判据的期望不写死机器）。`file beh5` 同样报 `dynamically linked`。

### 2.6 逐形 gcc 对照（7 夹具 × 4 档）

一次性脚本 `.work_tmp/meas3/cross.sh`（只读，不进仓库）：每个夹具分别用
dcc（宿主 cc 路径）与 `gcc -no-pie` 编 `-O0/-O1/-O2/-O3`，运行后比 stdout 与退出码。

| 夹具 | -O0 | -O1 | -O2 | -O3 |
|------|-----|-----|-----|-----|
| `attr_target_clones` | MATCH | MATCH | MATCH | MATCH |
| `attr_target_clones_cpu_builtin` | MATCH | MATCH | MATCH | MATCH |
| `attr_target_clones_cpu_matrix` | MATCH | MATCH | MATCH | MATCH |
| `attr_target_clones_cpu_is_matrix` | MATCH | MATCH | MATCH | MATCH |
| `attr_target_clones_select` | MATCH | MATCH | MATCH | MATCH |
| `attr_target_clones_ifunc_behavior` | MATCH | MATCH | MATCH | MATCH |
| `attr_target_clones_selected` | DIFF | DIFF | DIFF | DIFF |

**24/28 逐字相同**；4 个 DIFF 全部是 `attr_target_clones_selected`，差异只在
`delta=`/`has_*=`, 两处，原因即 §2.4 末尾那两条有意差异，`call`/`call3` 两侧一致
（`call=21`、`call3=17`）。

### 2.7 dcc 的 `.o` 可以直接交给宿主工具链

```
$ gcc .work_tmp/meas3/tc3.o -o o3exe ; echo rc=$?     ; rc=0
$ ./o3exe
compute(10) = 21
multiply(3, 4) = 12
```

这排除「只有 dcc 驱动的魔法才能链上」这一类假修。`ld` 给的 5 条警告都与 IFUNC 无关，
且对**任何** dcc `.o` 都成立（已用不含 clones 的 `plainstatic.c` 复核）：
`missing .note.GNU-stack section implies executable stack`（dcc 的 `.o` 一律没有这个节，
`readelf -SW tc3.o | grep -c note` = 0）与 `the use of 'tempnam'/'tmpnam'/'tmpnam_r' is
dangerous`（dcc 的 glibc 声明集把这些符号都当 UND 引出，`readelf -sW tc3.o | grep UND` 里
能看到 `stdin/stdout/remove/rename/tmpfile/tmpnam/...` 一整串）。两条都记在 §7.2。

---

## 3. `__builtin_cpu_supports` / `__builtin_cpu_is`：从恒假桩到真实现

### 3.1 接线（`src/codegen/x86_64/special/builtins.rs`）

名字表在 `src/preproc.rs`（`__builtin_cpu_init` / `_supports` / `_is`），发射在
`builtins.rs`，**实现全在 `codegen::x86_64::ifunc`**：

```
builtins.rs:2506  "__builtin_cpu_init"      → emit_cpu_init_builtin()（ifunc.rs:1154，
                                              置 cpu_builtin_used + call __dcc_cpu_indicator_init）
builtins.rs:2510  "__builtin_cpu_supports"  → self.emit_cpu_supports(a, span, dst)
builtins.rs:2520  "__builtin_cpu_is"        → self.emit_cpu_is(a, span, dst)
                  （缺参/非字面量各走 :1315 / :1326 的明确诊断分支，不静默返回 0）
ifunc.rs:1169  emit_cpu_supports   ┐
ifunc.rs:1236  emit_cpu_is         ├→ ifunc.rs:1377  emit_feature_check(arch, fail)
ifunc.rs:563   emit_cpu_indicator_init ┘              （唯一的一份判定发射器）
```

（行号为本轮末实测；`builtins.rs` 那三条是 `match name.as_str()` 的分支，
改那一侧的注释也同步过 —— 它原先写着「`.bss` 缓存」，与实际的 `.data` 不符，已改，
并把「为什么必须是 `.data`」写进注释。）

`emit_cpu_supports` 的形状是：查表（`cpu_isa::lookup`，不在表里 → 逐调用点 E1125
诊断 + 返回 0，**不**假装支持）→ `call __dcc_cpu_indicator_init` →
`emit_feature_check(name, fail)` → `mov $1; jmp end; fail: mov $0; end:`。
`emit_feature_check`（resolver 与 builtin 共用）做三件事：

1. 遍历 `cpu_isa::cpuid_bits(arch)`，每一位发
   `testl $1<<bit, SLOT(%rip)` + `je fail` —— **逐位**查，不聚合成掩码后只查非零
   （那会让「只置了其中一位」也判通过）；
2. 每位先过两道内部一致性检查：`cpu_isa::cache_slot(leaf, subleaf, reg)` 必须给出槽，
   `cpu_isa::probe_covers(...)` 必须证明 `PROBES` 里有一条探测填这个槽；否则报
   **E1126**（`DIAG_INTERNAL`，`ifunc.rs:103`）并返回 `false`，调用方**必须**发
   「返回 0」而不是「返回 1」——
   拿「CPU 支持该特性」冒充「编译器答不出」会让上层真去跑一条本机不支持的指令；
3. `cpu_isa::xcr0_mask(arch) != 0` 时补 `(XCR0 & mask) == mask`
   （`mov SLOT_XCR0_LOW; and mask; cmp mask; jne fail`）。

**自包含**是本任务书要求的：`.o` 里对 libgcc 的 UND 引用数为 **0**，gcc 的是 2。

```
$ LC_ALL=C readelf -sW tc3.o | grep -cE '__cpu_model|__cpu_indicator_init|__cpu.*UND'
0
$ LC_ALL=C readelf -sW tc3.gcc.o | grep UND
    14: … UND printf
    16: … UND __cpu_indicator_init
    18: … UND _GLOBAL_OFFSET_TABLE_
    17: … UND __cpu_model                     ; gcc 依赖 libgcc 的两个符号
```

dcc 这一侧是 `.o` 内的 LOCAL 符号：`__dcc_cpu_indicator_init` 3590 字节
（**8** 条 `cpuid` + **1** 条 `xgetbv`，见 §1.3 的探测序列）+ `__dcc_cpu_model` 100 字节。
探测序与槽位由 `cpu_isa::PROBES`/`SLOT_*` 单点决定，init 体里不写任何 leaf/reg/bit 数字。

### 3.2 幂等与「resolver 在 `.init` 之前被调」这件事怎么办

任务书要求二选一（体内联检测 vs `.init_array`）并说明理由。**dcc 选的是「体内联 +
惰性初始化」**：

* 每个用者（resolver 体的第一条指令、`emit_cpu_supports`/`emit_cpu_is` 的第一条指令）
  都先 `call __dcc_cpu_indicator_init`；
* init 体自己带守卫：进门先读 `SLOT_INIT_DONE`，非 0 立即 `ret`，跑完才写 1。
  于是重复调用零副作用，且**不依赖任何 `.init`/`.init_array` 已跑过** —— 缓存对象在
  `.data`（有镜像内容，初值全 0），`ld` 建完 GOT 才调 resolver，所以「读缓存」这件事
  在 resolver 被调用时一定是可安全读的；
* `__dcc_cpu_indicator_init` 是 `LOCAL`、只用 `R_X86_64_PC32` 引用同一个 `.data`，
  不经 GOT/PLT，也不引用任何 libc 符号。

为什么不走 `.init_array`（gcc 的另一半做法）：那条路依赖动态链接器**先**跑
`.init_array` 再解析 IRELATIVE，实测本机 `readelf -dW tc3` 里 `INIT_ARRAY` 是
`0x4051a0` —— 但 IRELATIVE 的处理在 `ld.so` 的 `resort_map`/`elf_machine_reloc`
阶段，顺序属于实现细节；把正确性押在它上面，不如让每个用者自带一次幂等调用。
gcc 的用户侧 `__builtin_cpu_init()` 也是同样形状的「显式调一次」，
`attr_target_clones_ifunc_behavior.c` 的手写 resolver 里就照这个写法调用它，两侧同形。

### 3.3 名字矩阵：与 gcc 逐字对照（60 + 51）

`attr_target_clones_cpu_matrix.c` 把 **60** 个 `__builtin_cpu_supports` 名字（dcc/gcc 的
已知名字全交集，含 `movdiri`/`pku`/`rdseed`/`serialize`/`clwb`/`wbnoinvd`/`avx512bf16` 等
容易写错位的那些）逐个打成 `name=<0|1>` 并求 `sum`；
`attr_target_clones_cpu_is_matrix.c` 同理打 **51** 个型号名。

```
$ diff m5.d m5.g && echo MATRIX_IDENTICAL      ; 61 行（60 + sum）
MATRIX_IDENTICAL        sum=45   本机 =1 的条目 45 个
$ diff ci.d ci.g && echo CPU_IS_IDENTICAL      ; 52 行（51 + sum）
CPU_IS_IDENTICAL        intel=1 / corei7=1 / rocketlake=1 三行 =1，sum=3
```

矩阵里几个关键读数（dcc 与 gcc 完全一致）：`sse4.2=1 avx=1 avx2=1 pku=1 rdseed=1
xsaveopt=1 abm=1 lm=1`；`movdiri=0 clwb=0 serialize=0 wbnoinvd=0 avx512bf16=0 avx512er=0`。

**本轮在采数时靠差分与不变量测试当场抓到的四个真错**（不是理论风险，是已修掉的既有
缺陷；`attr_target_clones_cpu_supports_matrix_matches_gcc` 的文档注释里也记着同一份名单）：

| 名字 / 对象 | 曾错写成 | 权威真值 | 为什么对照测试当时不红 |
|---|---|---|---|
| `rdseed` | (7,0):**EBX#22** | (7,0):EBX**#18** | 本机 #22=0、#18=1，而偏序链里没有 rdseed，四条不变量全绿、答案恒 0（gcc 报 1） |
| 扩展叶根探测 | 门控读 `SLOT_LEAF0_EAX` 并要求它 `>= 0x8000_0000` | 该探测**不得有门控** | 永假门控挡住 `0x8000_0001` 整条，`lzcnt/abm/lm/nx/syscall/rdtscp` 全答 0 |
| `movdiri` | (7,0):**EDX#27** | (7,0):ECX**#27** | 本机 ECX#27=0 而 EDX#27=1（那是 INTEL_STIBP），曾靠「把 movdiri 从对照夹具里排除」掩盖过一轮 |
| `serialize` | (7,0):**EBX#14** | (7,0):EDX**#14** | 那一位是 MPX；本机两者都 0，只有位号复核能发现 |

依据源：Linux `arch/x86/include/cpufeatures.h`（word 9 = (7,0)EBX、12 = (7,1)EAX、
16 = (7,0)ECX、18 = (7,0)EDX、1 = leaf1 EDX、6 = (0x8000_0001)ECX、13 = (0x8000_0008)EBX）
与 gcc `gcc/config/i386/cpuid.h` / `cpuinfo.h`（本机 gcc 13.3.0-6ubuntu2~24.04.1）。
`movdiri`/`serialize` 改正后重跑全夹具：`sum` 仍是 **45**、矩阵仍 `MATRIX_IDENTICAL`
（本机 `movdiri=0`、`serialize=0`，两侧一致）。

### 3.4 两个 builtin 都「不是恒假」的可证伪判据

* `attr_target_clones_cpu_builtin_is_real_implementation`：`.o` 里必须真有
  `__dcc_cpu_indicator_init`（LOCAL FUNC、size > 0）与 `__dcc_cpu_model`
  （LOCAL OBJECT 且坐 PROGBITS 的数据节），且对 libgcc 的 `__cpu_model`/
  `__cpu_indicator_init` UND 引用数必须为 **0**；
* `attr_target_clones_cpu_supports_matrix_matches_gcc`：60 名字四档逐字对照；
* `attr_target_clones_cpu_is_model_names_match_gcc`：51 名字逐字对照 **+** 与 Rust 侧
  参考求值器 `cpu_isa::cpu_is_holds` 逐项对账（§1.6）；
* `attr_target_clones_cpu_indicator_classify_ids_come_from_classify_table`：`.o` 字节级
  id 覆盖（§1.6，不运行任何代码）；
* `target_clones_runtime_resolves_to_non_default_variant`：builtin 读数与「解析到的变体
  地址」互为印证（§2.4）。

---

## 4. 内置链接器（`-fbuiltin-linker`）那条路径的实测

这条路径**仍然存在**，因为 `-fbuiltin-linker` 是显式请求；它必须继续明说链接方式（底线）。

```
$ dcc -O0 -fbuiltin-linker pure5.c -o /tmp/p5re ; rc=0
dcc: wrote /tmp/dcc-builtlink-1070140-0.o
dcc-link: static ET_EXEC: IFUNC 'compute' bound to variant 'compute.default' (no runtime
  resolver applier in this path -> CPUID dispatch is DISABLED; drop -fbuiltin-linker to
  get a dynamically linked binary that dispatches at load time)
dcc: built-in linker wrote /tmp/p5re (static (no PT_INTERP / DT_NEEDED), 13984 bytes, Executable)
                                          ↑ 三段播报齐全：降级原因 + 绑到哪个变体 + 链接方式

$ file /tmp/p5re       → ELF 64-bit LSB executable, x86-64, version 1 (SYSV)   （无 interpreter）
$ LC_ALL=C readelf -dW /tmp/p5re → There is no dynamic section in this file.
$ LC_ALL=C readelf -sW /tmp/p5re
     8: 00000000004001c8  100 FUNC   GLOBAL DEFAULT 1 compute           ← 与 compute.default 同址
    10: 00000000004010d0   41 FUNC   WEAK   DEFAULT 1 compute.resolver  ← 体仍在，只是没人调
    11: 0000000000403000  100 OBJECT LOCAL DEFAULT 3 __dcc_cpu_model
    12: 0000000000400160  100 FUNC   LOCAL  DEFAULT 1 compute.sse4_2
    13: 00000000004001c8  100 FUNC   LOCAL  DEFAULT 1 compute.default
    14: 00000000004002c0 3590 FUNC   LOCAL  DEFAULT 1 __dcc_cpu_indicator_init
$ LC_ALL=C readelf -lW /tmp/p5re | grep -E 'LOAD|GNU_RELRO'
  LOAD  0x003000 0x…403000 … 0x000064 0x000064 RW  0x1000
  GNU_RELRO 0x003000 0x…403000 … 0x000064 0x000064 R 0x1000
$ /tmp/p5re ; echo rc=$?   → rc=0
```

`GNU_RELRO FileSiz = 0x64 = 100` 与 `__dcc_cpu_model` 的 `st_size` 逐字节相等：
这是 §1.4「缓存必须坐 `.data` 而不是 `.bss`」的正面证据（内置链接器不映射 NOBITS，
反面基准 `static int buf[8];` 走这条路径实测 **rc=139** SIGSEGV）。

两个已知、且都**不是**本轮引入的点：

* `compute` 在这条路径绑到 `compute.default`（这条路径没有 IRELATIVE 应用器，
  CPUID 派发被明说为 DISABLED）—— 记入 §7.1 的有意边界；
* `readelf` 警告 `local symbol 14/15/16 found at index >= 's sh_info value of 14`：
  symtab 的 `sh_info`（局部符号数）算少。**已复核为既有缺陷、与 IFUNC 无关**：
  不含任何 clones 的 `plainstatic.c`（一个 `static` 函数）走同一条路径也给
  `local symbol 10/11 found at index >= sh_info value of 10`。计数在
  `src/link/layout.rs:132`（「在 .symtab 中所有 LOCAL 符号数（用于 sh_info）」），
  `layout.rs` 不在本包所有权内 → §7.2。


---

## 5. 三态判据的翻转（这是任务书第 4 条要求的「翻」）

判据原名 `target_clones_diverts_to_static_link_is_known_defect`（三态哨兵），
现名 **`target_clones_links_dynamically_with_codegen_resolver`**
（`tests/integration/c_attr_builtin.rs:482`）。它的文档注释（`:456-479`）写的就是这一翻：
「旧形状…哨兵当时停在『链不上』这一格，并写明『真做出动态产物后必须翻成断言动态』——
resolver 搬回 codegen 后改道分支已删，本用例就是那一翻」。

三态的三格与实测归位：

| 格 | 形状 | 上一轮 | 本轮 | 本轮依据 |
|----|------|--------|------|----------|
| A | 链不上（几十个 E0302） | 哨兵停在这里 | **不再成立** | `dcc -O0 attr_target_clones.c` → rc=0（§2.1） |
| B | 链上但是**静态** | 改道内置链接器时成立 | **不再成立** | `file` 报 `dynamically linked`、有 `INTERP` 段、`NEEDED libc.so.6`（§2.1） |
| C | 链上且**动态** + IFUNC 语义活着 | 没做到 | **成立** | 两条 `R_X86_64_IRELATIVE`；exe 里 `compute` = `IFUNC`/`GLOBAL` 且与 `compute.resolver`（`FUNC`/`WEAK`）同 `st_value` 同 `st_size`（§2.1） |

翻的依据必须落在**外部工具的读数**上，所以判据里 `assert_dynamically_linked`
（`c_attr_builtin.rs:433-454`）一次收三项而不是一项：`.interp` 节存在（= 有 `PT_INTERP`）、
`DT_NEEDED` 里有 `libc.so.6`、`file` 输出含 `dynamically linked`。
理由是「静默降级为静态」的形状正是三者同时缺，只断其一会被
「有 `.interp` 但没有 NEEDED」这类半成品骗过。

判据的五个断言步骤（顺序即防假绿的设计）：

1. **基准线**：不带 IFUNC 的同主程序（`plain`）必须先断 `assert_dynamically_linked` ——
   翻转三态判据最容易造成的假绿是「所有产物都变静态而判据跟着改」，这一步让那种
   改法立刻红；
2. 带 IFUNC 的 `clones` 必须**链得上**（旧形状在这里是 E0302）；
3. 产物必须 `assert_dynamically_linked`；
4. stderr 必须含 `via host cc`（底线：路径不得静默；内置链接器那条改道分支不该再出现）；
5. exe symtab 里 IFUNC/resolver 的 `stype`/`bind`/`(value,size)` 逐条对（`:522-530`）。

地址漂移的说明（写进判据注释，也写在这里）：注释里记的
`405318 / 4030c0` 是**判据自建 exe** 的读数，本报告 §2.1 记的是
`4053b8 / 4032b0`，两者不同是因为构建入口不同（`dcc_link_exe` 走临时 `.o` + 宿主 cc，
手工采数走 `dcc -O0 src -o exe`），`__dcc_cpu_indicator_init` 占 3590B 也会让
`.text` 尾部整体漂。**不变量**是「IRELATIVE 存在 + IFUNC 与自己的 resolver 同址同尺寸」，
不是绝对地址；判据断的是不变量。

同一族的另外两条（都在 `target_clones` 的 16 条里）：

* `target_clones_link_mode_is_announced_on_both_paths` —— 同时钉两条路径的**措辞**
  （宿主 cc 侧 `via host cc`、内置侧 `static (no PT_INTERP / DT_NEEDED)`），
  这一条是「不得回到静默」的直接守卫；
* `target_clones_builtin_linker_static_binds_default_and_announces` —— 钉 §4 那个形状
  （静态、绑 default、明说派发被禁用），也就是「内置链接器路径仍在」这件事的守卫。

---

## 6. 单一真源是怎么保证的

### 6.1 唯一的一份规格在 `src/semantics.rs` 的 `pub mod cpu_isa`（`:768` 起）

| 内容 | 位置 | 谁是它的读者 |
|------|------|--------------|
| `IsaFeature` / `FEATURES`（名字 → (leaf, subleaf, reg, bit) + XCR0 需求 + 优先级） | `:792` / `:930` | `cpuid_bits`/`xcr0_mask`/`dispatch_priority`/`lookup` |
| `dispatch_priority` / `dispatch_order` | `:1190` / `:1199` | codegen 排判定链、内置链接器排判定链 |
| `SLOT_*` + `SLOT_WORDS(25)` + `MODEL_BYTES` | `:1214-1252` | init 体的写槽、`emit_feature_check` 的读槽、`__dcc_cpu_model` 尺寸 |
| `PROBES`（CPUID/xgetbv 探测序列 + `Gate`） | `:1290` | init 体发射、`probe_covers` |
| `cache_slot(leaf, subleaf, reg)` | `:1431` | 任何要读某 leaf/reg 位的发射方 |
| `VENDOR_SIGS` | `:1551` | init 体里的厂商 dword 比较、`vendor_kind_of_bx` |
| `CLASSIFY`（family/model → type/subtype + 特性守卫） | `:1735` | 分类链、`type_id`/`subtype_id`、`classify` |
| `CPU_IS_NAMES`（51 个 gcc 13.3 名字 → 比哪个槽/比什么值） | `:2063` | `emit_cpu_is`、`cpu_is_holds` |
| `host_supported` / `host_processor_class`（Rust 侧同一份表的自检） | `:1586` / `:2214` | 交叉核对判据的参考求值器 |

### 6.2 三个消费方，零份重复

* **codegen 的 resolver 体**（`src/codegen/x86_64/ifunc.rs`）：对 `cpu_isa::` 的引用
  **107** 处。发射侧不写任何 leaf/bit/槽号：`emit_cpu_indicator_init` 逐条消费
  `cpu_isa::PROBES`（`:574`），`emit_feature_check` 逐位走
  `cpuid_bits`+`cache_slot`+`probe_covers`+`xcr0_mask`（`:1377+`），OSXSAVE 门控位从
  `cpu_isa::osxsave_test()` 反查（`:588`，注释里明写「不在这里写 `1 << 27`」）。
* **内置链接器**（`src/link/target_clones.rs`）：`cpu_isa::` 引用 **18** 处，位表部分
  只剩两个薄适配器 —— `arch_to_cpuid_bits`（`:76`，转发 `cpu_isa::cpuid_bits`）与
  `arch_xcr0_mask`（`:85`，转发 `cpu_isa::xcr0_mask`）。全文件里 `1 << 27` 只出现在
  **注释**中（`:282`「旧版在这里硬写 `1 << 27`，位表就有两份」、`:724`/`:728` 是测试里
  对字节流的描述），没有任何一处代码自带 leaf/bit 常量。
* **`__builtin_cpu_supports` / `_is`**（`src/codegen/x86_64/special/builtins.rs`）：
  `cpu_isa::` 引用 **0** 处 —— 它只把三个名字转发到 `emit_cpu_supports`/`emit_cpu_is`/
  `emit_cpu_init_builtin`，语义全部由 `ifunc` 那侧的同一张表决定。

交叉验证（本轮重跑）：`grep -c 'cpu_isa::'` 在上述三个文件分别是 107 / 18 / 0；
`grep -nE "1 *<< *[0-9]" src/link/target_clones.rs src/codegen/x86_64/ifunc.rs` 命中的
三行全是注释。也就是说**「两处各写一份位表」这一件事在源码层面可证伪、且当前为假**。

### 6.3 内置链接器那条既有路径为什么仍要保留（`src/link/target_clones.rs:18-40`）

原文三条理由（代码注释，不是报告里的补充说明）：

1. **输入形状不同**：`collect_link_directives` 会从符号表反推 target_clones 函数
   （含 `.dcc.ifunc` 显式指令那条路径，以及外部提供的、只带变体符号**不带 resolver**
   的 `.o`）；这种输入里没有「对象内的 init 函数」可调，只能在链接期凭变体列表现拼一个；
2. **定位方式不同**：链接期拼出的 resolver 落在 `.text.ifunc`，`MOV RAX, imm64` 的
   立即量是**最终镜像 vaddr**（`patch_all_resolvers` 回填），因为静态镜像没有
   `.rela.text` 可依赖；codegen 侧的 resolver 用 `R_X86_64_PC32` 交给宿主 `ld` 解析。
   两种不能互换，所以两段体各自发射；
3. **规格只有一份**：leaf/bit、XCR0 需求、变体优先级全部来自 `cpu_isa`，本模块不再
   自带位表 —— 否则「内置链接器选的变体」与「宿主链接器选的变体」会因两张表漂移而不同，
   那正是本项目禁止的分阶段缺口。

### 6.4 钉住「单一真源」的不变量测试（本轮全绿，lib 侧相关共 **60** 条：26 + 18 + 9 + 7）

* `semantics::cpu_isa::tests` **26** 条（`cargo test --lib -- cpu_isa` 报 28 passed，
  多出的 2 条是下面名字里含 `cpu_isa` 的 link 侧测试）。与本轮设计直接相关的：
  `every_bit_has_a_cache_slot`、`every_bit_is_covered_by_probe_sequence`、
  `probe_slots_are_within_model_size`、`probe_gates_are_satisfiable`（防「永假门控」——
  扩展叶根那一格本轮真抓到过）、`subleaf_slots_are_distinct_from_subleaf_zero`、
  `slot_masks_aggregate_per_slot`、`dispatch_order_matches_gcc_measured_chain`、
  `cloneable_set_matches_gcc13`、`cpu_is_names_equal_the_gcc13_measured_set`、
  `classify_every_id_is_addressable`（防「发得出但问不到」的 id）、
  `class_ids_are_unique_and_derived`、`decode_formula_matches_gcc_for_each_vendor`、
  `classify_chain_semantics_on_synthetic_inputs`、
  `table_agrees_with_host_detector` / `host_cpu_is_agrees_with_gcc13_measured_answers` /
  `host_reference_gates_subleaves` / `host_reference_reads_extended_leaves`
  （Rust 侧自检 vs 表）、`vendor_dwords_rebuild_the_vendor_strings` /
  `vendor_dwords_agree_with_host_leaf0`、`osxsave_gate_bit_is_the_table_entry`、
  `avx_family_needs_xcr0`、`lookup_normalizes_underscore_and_case`、
  `names_are_unique_after_normalization`、`classify_guards_exist_in_feature_table`、
  `classify_predicates_are_well_formed`、
  `cpu_is_names_are_mutually_exclusive_and_imply_types`；
* `link::target_clones::tests` **18** 条，其中两条是真源守卫：
  **`bit_table_is_the_shared_cpu_isa_source`**（链接期 blob 里的 leaf/bit 必须等于
  `cpu_isa::cpuid_bits`，不许另有来源）、
  **`dispatch_order_follows_cpu_isa_priority_not_declaration_order`**；
  另有 `linker_path_cloneable_bits_have_no_subleaf`（内置路径消费的位集里不得出现
  subleaf≠0 的位，见 §1.4）与 `emit_cpuid_leaf_caches_by_leaf_and_subleaf`；
* `link::ifunc::tests` **9** 条（静态路径绑定与 `.dcc.ifunc` 节的解析/截断/错误路径）；
* `codegen::x86_64::ifunc::tests` **7** 条，其中
  **`feature_check_reads_only_table_derived_slots`** 断发射器只用表派生的槽，
  `unknown_isa_name_is_an_error_not_a_silent_never_matching_check` 断「未知名字必须报错」
  而不是发一段永不命中的检测。

`link::target_clones::tests::bit_table_is_the_shared_cpu_isa_source` 的实现方式值得记一句：
它把模块内 `arch_to_cpuid_bits("sse4.2"/"avx2"/…)` 的结果与
`cpu_isa::cpuid_bits(...)` **逐元素比对**，并额外断 `arch_to_cpuid_bits("default")` 为空 ——
于是「在 link 侧另写一份表」会在编译期之外的第一手数据上就红。

### 6.5 一件「看起来不相关但必须同处做」的事：resolver 不能被 DCE 删掉

`__attribute__((ifunc("r")))` 的 `r` 只被属性引用、不被任何 C 调用点引用，
所以可达性 DCE（`ssa_passes::cross_function_dce`）会把它当死代码删掉；实测 gcc 13 在
`-O2/-O3` 仍发 `FUNC LOCAL resolve_a` + `IFUNC GLOBAL ifunc_a` 同址。dcc 的做法是
`expand_target_clones` 的第 0 步 `retain_ifunc_resolvers`（调用点
`src/codegen/x86_64/ifunc.rs:276`，定义 `:460`）
把 resolver 标成 `is_used_attr` —— 那正是那条 DCE 认的保留位（roots 与 retain 两个判定点
都读它）。**这样就不必在 `src/opt.rs` / `src/ssa.rs` 里另列一份名字白名单**，
也避开了本包不能碰那两个文件的约束（四档 28 组逐形对照全绿是这件事的证据，§2.6）。


---

## 7. 仍未闭合的点

### 7.1 有意的边界（都在代码注释里写明，不是「默默无效」）

1. **没有 per-variant 的 ISA codegen** —— 同一个函数的各变体机器码**逐字节相同**（本轮用
   `cmp` + `dd` 直接比字节，`.o` 与 exe 两个层面 × 两支函数链共四组比较全部相等）：
   * `.o`（`attr_target_clones.c`，`.text` 节偏移 0x40，`st_size` 十进制）：
     `compute.sse4_2`@0x0 与 `compute.default`@0x68 各 **100 字节**（`0x64`）逐字节相等；
     `multiply.avx`@0xd0 / `.sse4_2`@0x128 / `.default`@0x180 各 **82 字节**（`0x52`）
     两两相等。两具之间的 4 字节是 16 字节对齐填充（`90 90 90 90`），也一并比了；
   * exe（`sel.d`，vaddr − 0x400000 = 文件偏移）：`compute.sse4_2`@0x402150 与
     `compute.default`@0x4021b8 各 **100 字节**相等；`dispatch3.avx`@0x402220 /
     `.sse4_2`@0x402270 / `.default`@0x4022c0 各 **80 字节**两两相等。
   （四组都是**按 `st_size` 全量**比较，不是截前缀 —— 中途一版曾用 `objdump -t` 的十六进制
   尺寸字段当十进制，写成「`.o` 各 64/52 字节」并把比较截成前缀，本轮按 `readelf -sW` 的
   十进制 `st_size` 重做并复核：结论不变，数字改准。）
   根因：`src/ast.rs` 的 `FuncDecl` 只带 `target_clones`/`ifunc_resolver`（本轮未改
   `ast.rs`），展开时把**同一个体**复制 N 份，没有把「这一份要用哪套 ISA」传进指令选择层。
   后果与本 WP 的关系：**选型无法用 stdout 观察**，只能用（a）解析后的实现体地址
   （§2.4）或（b）`__attribute__((ifunc))` + 手写不同实现体（§2.5）来钉。
   为什么不在本包做：那要改 `codegen.rs`/`instructions.rs`/`emit/**` 的函数级 ISA 上下文
   并回归整个语料，与「IFUNC 使链接静默降级」这个缺陷本身无关（那条缺陷修的是**链接方式**，
   不是变体代码质量）。
2. **静态（内置链接器）路径不做运行时派发**：IFUNC 绑到 `<base>.default` 并**无条件**播报
   （`src/link/mod.rs:550-575`）。为什么不能「静态也运行时派发」已写在同一段注释里：
   需要 GOT 间接 + 启动期重定位应用者 + ET_EXEC 里真写出重定位节，三件分别落在
   `layout.rs`/`plt_got.rs`/`elf_exec.rs`（本包不拥有）；逐项缺口在 `src/link/ifunc.rs`
   模块头。放任 IFUNC 指向 resolver 的实测后果是 SIGSEGV（`call compute` 跳进 resolver、
   `ret` 把函数指针当返回值），所以绑 default 是唯一对任何 CPU 都合法的做法。
3. **Zhaoxin `lujiazui` 那一支少一个 gcc 的副作用**：gcc `cpuinfo.h` 在分类后还会
   `reset_cpu_feature(FEATURE_AVX/FEATURE_F16C)`（lujiaZui 报 AVX 但不真执行 VEX）。
   dcc 的分类链**只写 type/subtype，没有「分类结果反向清除特性位」这个机制**
   （`src/semantics.rs:1880-1884` 的注释明写此事），所以在那类 CPU 上
   `__builtin_cpu_supports("avx")` 会与 gcc 飘。本机（Intel）观察不到，因此没有判据能钉它；
   要闭合需要给 `CLASSIFY` 加 `clears: &[&str]` 一类的字段并让 init 体清位。
4. **本表故意不含 AMD family 1Ah（znver5）那一组**，且 Zhaoxin 组的 `group_type` 是空串：
   两者都是「gcc 13.3 的对外名字表里没有名字能问到这个结果」，而不变量
   `classify_every_id_is_addressable` 不允许存在「发得出但问不到」的 id
   （`src/semantics.rs:1862-1867`、`:1876-1879`）。要支持 gcc≥14 的名字集时必须
   **同时**往 `CPU_IS_NAMES` 加 `znver5`/`amdfam1ah`。
5. **有意与 gcc 13.3 的两处错配不一致**：13.3 把 `shanghai`/`istanbul`/`bdver1`/`bdver2`
   发成「比较 `__cpu_type`」，而 `cpuinfo.h` 那边它们只出现在 `__cpu_subtype`（gcc 15.3
   已改成 `M_CPU_SUBTYPE`）。dcc 按语义正确的那份实现比 subtype
   （`src/semantics.rs:2057-2062`）：本机两侧答案逐字相同（都是 0），但在真的
   Shanghai/Bulldozer 机器上这几个名字会与 gcc 13.3 飘 —— 不静默复制别人的 bug。
6. **仿冒厂商串**：发射侧比较三个 vendor dword（`VENDOR_SIGS`），而
   `vendor_kind_of_bx`（`:1981`）只比 BX（与 gcc `cpu_indicator_init` 同形）。
   差别只在「BX 相同但 CX/DX 不同」的机器上：dcc 会判 `VENDOR_UNKNOWN`（什么都不分类），
   gcc 会落到某一厂商分支。该函数不参与发射，只服务「按 BX 查 kind」的调用方。
7. **`__builtin_cpu_supports` 返回 0/1，不是 libgcc 的位值**（本机 gcc 给
   sse4.2=256、avx=512）：文档只承诺 non-zero，判据一律 `!= 0`（§2.4）。
8. **`.o` 的 `EI_OSABI` 仍是 0**，所以 `readelf -s` 在 `.o` 上把 `st_type=10` 打成
   `<OS specific>: 10`（中文 locale 再译一层），肉眼 grep `IFUNC` 会假阴性；
   exe 那侧由 `ld` 设成 GNU 所以正常显示。写入点不拥有 → 下面第 1 条。

### 7.2 需要主控落地的跨所有权改动（本包已核过、但改不到）

| # | 文件 / 位置 | 现象（实测） | 建议 |
|---|-------------|--------------|------|
| 1 | `src/codegen/shared/elf.rs:374`、`:1484`、`:2520`、`:2743`、`:2788`（五处 `out.push(0); // ei_osabi`） | dcc `.o` 的 `OS/ABI: UNIX - System V`，gcc `.o` 是 `UNIX - GNU`；后果是 `readelf` 在 `.o` 里把 `STT_GNU_IFUNC` 打成 `<OS specific>: 10`。**语义无碍**：把第 7 字节手工改成 3 后同一文件立刻打 `IFUNC`，且宿主 `ld` 照样链通（`gcc tc3_osabi3.o` → rc=0，exe 的 OS/ABI 变 GNU） | 改成 `ELFOSABI_GNU(3)`（或至少在有 IFUNC/OS 特定符号时设 3），使 `.o` 侧判读与 gcc 一致 |
| 2 | `src/cpuid.rs:359`（`avx_vnni = bit(ecx7, 4) && feats.avx`） | 与 `cpu_isa::FEATURES` 对账时的**唯一真错位**：AVX_VNNI 的真值是 CPUID.(7,**1**):EAX#4（Linux `cpufeatures.h` word 12），`(7,0):ECX#4` 那一位是 **OSPKE**；而且它的守卫应是 XCR0 而非 `feats.avx` | 按 `cpu_isa::lookup("avx_vnni")` 的表项改（表已是真源，直接消费它即可） |
| 3 | `src/cpuid.rs`（`detect_x86()` 的名字集） | `detect_x86()` **根本不报告** `serialize` 与 `clwb` 这两个名字（不是报错位，是没有字段）。所以 `semantics.rs` 里的 `table_agrees_with_host_detector` 只比**交集**，其内部注释已单独记了 `avx_vnni` 不参与比对的理由 | 补上这两个名字后，把交叉核对测试的名单扩到全集 |
| 4 | `src/link/layout.rs:132`（`.symtab` 的 `sh_info` = LOCAL 符号数） | `-fbuiltin-linker` 的产物有 `readelf: Warning: local symbol 14/15/16 found at index >= sh_info value of 14`。**与 IFUNC 无关**：不含任何 clones 的 `plainstatic.c`（一个 `static` 函数）也给 `local symbol 10/11 … sh_info value of 10` | 修 `sh_info` 的计数口径（局部符号排在前面，计数要含所有 `STB_LOCAL` 条目，含 `SECTION`/合成的那几条） |
| 5 | dcc 的 ELF 输出（节表侧；`src/codegen/x86_64/elf/output.rs` 在本包内，但节表布局与 `-S` 同形、`shared/elf.rs`、全语料节索引都受影响） | **每一个** dcc `.o` 都缺 `.note.GNU-stack`：`readelf -SW tc3.o \| grep -c note` = 0，宿主 `ld` 因此警告 `missing .note.GNU-stack section implies executable stack`（非 PIE 的 `plainstatic.o` 同样触发）。gcc 的 `.o` 有 `[13] .note.GNU-stack PROGBITS` | 统一补一个 non-exec `.note.GNU-stack` 节（属主需要一次全语料的节索引核对，所以不在本包顺手改） |
| 6 | `src/codegen/disasm.rs`（**禁列**） | `dcc -S` 渲染合成体时：`cpuid`（`0f a2`，8 处）与 `xgetbv`（`0f 01 d0`，1 处）都打 `# UNKNOWN …`；150 处 RIP 相对操作数一律渲染成 `__dcc_cpu_model(%rip)`，**丢掉 `+disp`**，于是读不同槽的指令在 `-S` 里看起来完全一样。objdump 读同一份 `.text` 是 `cpuid` / `xgetbv` / `# 4053e8 <__dcc_cpu_model+0x18>` | 给 disasm 补这两条助记符 + RIP `+disp` 的符号化；在此之前「`-S` 读数不可用于判断机器码语义」这条项目规则对本模块同样生效（本报告的机器码判读全部来自 `objdump -d`） |
| 7 | `docs/ssa-pass-upgrade-plan.md` §7.38 / §7.41 三（**禁列**） | 还写着「IFUNC 使链接静默从动态降级为静态」 | 改成已修，并指向本报告；三态判据的新名字与五项终态可直接从 §0 表抄 |
| 8 | `docs/missing_features.md` 的 IFUNC 行（**禁列**） | 同上 | 同上（`__builtin_cpu_supports`/`_is` 两行如仍标「桩/未实现」也应一并翻） |
| 9 | `Makefile` / `scripts/**`（**禁列**） | 本包新增 6 个夹具都是 `.c` 文件、判据都加在已被 include 的 `tests/integration/c_attr_builtin.rs` 里，因此**不需要**新 test target；但若要把 `attr_target_clones_*.c` 纳入某个语料级 target，需主控决定 | 无需为本次改动动 `tests/integration.rs` 壳（已核：它 `include!` 的列表未变） |
| 10 | `src/opt.rs` / `src/ssa.rs`（**禁列**） | **不需要改**：resolver 存活问题用既有的 `is_used_attr` 保留位解决（§6.5），没有在优化器里加名字白名单 | 无动作；记录这一点是为了说明「没改禁列文件」不是回避 |

## 8. 判据清单（用例名 + 它钉哪一条终态）

### 8.1 跑法与总账

```bash
export CARGO_TARGET_DIR=/root/dcc/.work_tmp/tgt_ifunc
nice -n 19 cargo test --release -j1 <filter>     # 一律 -- --test-threads=1
```

| filter | 通过 | 失败 | rc |
|--------|------|------|----|
| `--lib -- cpu_isa` | 28 | 0 | 0 |
| `--lib -- link::target_clones` | 18 | 0 | 0 |
| `--lib -- link::ifunc` | 9 | 0 | 0 |
| `--lib -- codegen::x86_64::ifunc` | 7 | 0 | 0 |
| `--test integration -- target_clones` | 16 | 0 | 0 |
| `--test integration -- cpu` | 25 | 0 | 0 |
| `--test integration -- ifunc` | 5 | 0 | 0 |

两个口径提醒（避免把上面的数字当总数读）：

* lib 侧 filter 是**子串**匹配，彼此重叠：`cpu_isa` 那 28 = `semantics::cpu_isa` 的 26 条
  + `link::target_clones::tests::bit_table_is_the_shared_cpu_isa_source` +
  `…dispatch_order_follows_cpu_isa_priority_not_declaration_order`。**去重后的本包 lib
  判据是 60 条**（26 + 18 + 9 + 7，§6.4 同一个数）。
* integration 侧 filter 带出 39 条与本模块名字相邻的用例，其中 **20 条不是本包写的**
  （§8.7），列出来只为了说明「本包没把它们改坏」。

所有与 gcc 的逐字对照（矩阵、行为级、四档）都是**测试自己跑宿主 gcc** 做差分，不依赖
本报告里抄下来的读数；本轮另外用当前构建手工复采了一遍（`.work_tmp/meas3/recheck/`），
六支夹具的输出与 §1-§5 的判据全部对上。

### 8.2 终态 1（`.o` 里 IFUNC 的 `st_value` 指向真 resolver 体）

| 用例 | 钉什么 |
|------|--------|
| `attr_target_clones_ifunc_symbols` | 基名 `st_type` 必须是 `STT_GNU_IFUNC(10)`，且 `st_value`/`st_size` 与 `<base>.resolver` **完全一致**、与各变体实现体地址**互不相等**（直接钉旧形状「IFUNC 与实现体同址」不可能再发生） |
| `attr_target_clones_resolver_bits_come_from_cpu_isa_table` | resolver 的**机器码本身**逐位来自 `cpu_isa`：按 `F7 05`/`8B 05`/`C7 05` 等发射形式从 `r_addend + (L - f)` 还原「槽 + 立即数」，与 `cpu_isa::cache_slot(leaf,reg)`/`bit` 逐条比对；`xcr0_mask != 0` 的 ISA 必须能看到读 `SLOT_XCR0_LOW`，SSE 系必须看不到 |
| `attr_target_clones_select_resolver_dispatch_order` | 判定链按 **ISA 优先级表**排（`avx512f → avx → sse4_2 → default`），不是按 `target_clones(...)` 的书写顺序 |
| `codegen::x86_64::ifunc::tests::expansion_emits_one_body_per_variant_and_a_bodiless_base` | 展开形状：每变体一份独立体 + 基名**无体** |
| `…::variant_symbol_name_matches_gcc_measured_shape` | 变体符号名（`f.sse4_2` 而非 `f.sse4.2`）与 gcc 实测同形 —— 写错就链不到 |
| `…::feature_check_reads_only_table_derived_slots` | `emit_feature_check` 只能读表导出的槽，不许硬编码偏移 |
| `…::unknown_isa_name_is_an_error_not_a_silent_never_matching_check` | 拼错的 ISA 名报 **E1125**，不得退化成「永不匹配的静默检查」 |
| `…::missing_default_is_an_error` / `…::single_variant_is_a_warning_and_stays_a_normal_function` / `…::nested_function_in_cloned_body_is_rejected` | 三种输入形状的诊断口径（不是静默） |

### 8.3 终态 2（宿主 `cc`/`ld` 原生吃下、产动态、与 gcc 同果、选型可观察）

| 用例 | 钉什么 |
|------|--------|
| `target_clones_links_dynamically_with_codegen_resolver` | §8.5（三态翻转后的判据） |
| `target_clones_runtime_resolves_to_non_default_variant` | `(void *)compute` 与 marker 的**地址差**必须等于「`__builtin_cpu_supports` 说支持的那个变体」的地址差：本机 `delta=-448` ⇒ `compute.sse4_2`（default 那支是 -344）、`delta3=-240` ⇒ `dispatch3.avx`（sse4_2 -160、default -80）。恒假桩 / 恒返 default / 恒返第一个变体三种偷工都得 rc != 0 |
| `attr_target_clones_ifunc_behavior_selects_non_default` | **行为级**：`__attribute__((ifunc))` + 返回值不同的两个实现体（`fast_impl=2`/`slow_impl=1`），resolver 只在 leaf1/EDX#26（x86-64 恒真）时返回 `fast_impl`；`by_sse2` 必须得 **2**。与 §2.5 实测一致 |
| `ifunc_resolver_runtime`（`c_link_debug.rs`） | 同一夹具交给**宿主 gcc** 直链也要同果（PIE 形状：`IFUNC @0x2260 == resolve_sse2 @0x2260`、两条 IRELATIVE），并断产物仍是动态 |
| `target_clones_libc_links_via_host_cc_and_builtin_failure_is_attributed` | `target_clones` + `printf` 必须能经宿主 cc **链上**（旧形状在这里堆几十个 E0302）；用户显式 `-fbuiltin-linker` 时依旧链不上，但那串 E0302 必须**归因到内置链接器**而不是让用户误读成「缺 libc 符号表」 |

### 8.4 终态 3（两个 builtin 是真实现）

| 用例 | 钉什么 |
|------|--------|
| `attr_target_clones_cpu_builtin_is_real_implementation` | 四条机器无关不变量：非空（不是恒 0）、偏序（`avx2 ⇒ avx ⇒ sse2`）、互斥（`intel` 与 `amd` 不同时真）、`__builtin_cpu_init()` 后幂等。恒 0 桩钉不住第一条，硬编码 1 钉不住第二、三条，拼接顺序写错（ECX 当 EDX）钉不住第四条。四档 `-O0/-O1/-O2/-O3` 都与 gcc 逐字比 stdout；同一用例还钉「自包含」：**只用** builtin（夹具里不带 `target_clones`）时 `.o` 里也必须发 `__dcc_cpu_indicator_init` + `__dcc_cpu_model`、不得引用任何 libgcc 符号（`__cpu_model`）、且 `.rela.text` 里真的有指向这两者的重定位（= 读表序列真的在 `.text` 里，不是“有个定义”就完） |
| `attr_target_clones_cpu_supports_matrix_matches_gcc` | **60 个名字逐个问**、四档与 gcc 差分（本机 61 行输出逐字相同、`sum=45`）。名字表是 gcc13 `i386-isas.h` 与 dcc `cpu_isa::FEATURES`(+`FEATURES_TAIL`，共 72 条) 的**交集**，并用 `assert_eq!(NAMES.len(), 60)` 按数量钉住；同一用例还钉：每个名字在输出里**必须恰好出现一次**（缺行 = 夹具与名字表脱钩）、值必须归一为 0/1、行数必须正好 60+1、`sum=` 与逐行 1 的个数自洽；以及 x86-64 恒真锚点 `lm`/`mmx`/`cmov`/`sse`/`sse2` 必须读 1（`lm` 读 0 就是扩展叶整条没跑）、`abm == lzcnt`（同一位的两种写法） |
| `attr_target_clones_cpu_is_model_names_match_gcc` | **51 个型号名**逐个问、四档与 gcc 差分（本机输出 52 行 = 51 名 + `sum=3`，与 gcc 逐字相同；本机为 1 的只有 `intel`/`corei7`/`rocketlake` 三行）；两层设计避免「把本机答案抄进断言」：换 AMD/Zhaoxin 机器期望跟着变而判据仍成立（型号槽的 id 来源另由下一条钉） |
| `attr_target_clones_cpu_indicator_classify_ids_come_from_classify_table` | `__dcc_cpu_indicator_init` 写进型号槽的 **imm32 必须恰好等于 `CLASSIFY` 里的 id**（`C7 05`、`delta = 8` 还原加数）。这条专治「在本机那条 Intel 路径上看起来对、换个厂商就静默错位」的假修 |
| `cpu_builtin_supports_runs_under_builtin_linker_static` | 静态（内置链接器）产物里 `__builtin_cpu_supports` 也必须真跑 —— 这条钉住本轮把 `__dcc_cpu_model` 从 `.bss` 换到 `.data` 的决定（那条路径不映射 NOBITS，留 `.bss` 会 SIGSEGV） |
| `semantics::cpu_isa::tests`（26 条，§8.6） | 位表/探测/分类表自身的闭合性 —— builtin 的返回值全部由它推导，没有第二份表 |
| `builtin_cpu_compiles`（既有） | 三个 builtin 在旧用例里就只能编过；本轮把它扩成能跑之后，它退化为编译性护栏 |

### 8.5 终态 4（三态判据翻转）+ 终态 5（底线不退）

| 用例 | 钉什么 |
|------|--------|
| **`target_clones_links_dynamically_with_codegen_resolver`** | 前身 `target_clones_diverts_to_static_link_is_known_defect`（三态哨兵，停在「链不上」那一格，并写明「真做出动态产物后必须翻成断言动态」）。现在断：`assert_dynamically_linked`（`.interp` + `DT_NEEDED libc.so.6` + `file` 报 dynamically linked 三项）+ stderr 有 `via host cc`（= **没**走内置链接器）+ `readelf -rW` 有 `R_X86_64_IRELATIVE` + `readelf -sW` 里 IFUNC 与自己的 resolver 同址同尺寸 + 运行 stdout 与 gcc 逐字相同。三态哨兵的名字在 `src/`、`tests/` 里已全部消失（只在两处历史叙述里出现：本用例的 doc 注释、§5） |
| `target_clones_link_mode_is_announced_on_both_paths` | 底线 5 正面：两条链路的**链接方式**都必须真实且自报（宿主链 → `via host cc`；内置链 → `static (no PT_INTERP / DT_NEEDED), N bytes`）。前身钉的是「产物是静态的 + 明说」，现在产物是动态的，所以翻成「两侧各自报对」 |
| `target_clones_builtin_linker_static_binds_default_and_announces` | 底线 5 反面：用户**显式** `-fbuiltin-linker` 时 IFUNC 绑 `<base>.default`、跑得出、且**无条件**播报「CPUID dispatch is DISABLED」。它同时也是「不许回到静默」的护栏 |
| `assert_dynamically_linked` 的基准线用法 | 上述判据对**不带 IFUNC** 的基准夹具同样调用（翻转三态判据最容易造成的假绿是「所有产物都变静态而判据跟着改」，所以基准线必须一起断） |
| `m27_attr_ifunc_symbol_type_is_ifunc` / `m27_attr_ifunc_with_builtin_linker`（既有） | `__attribute__((ifunc))` 的符号类型与内置链接器入口不退化 |

### 8.6 钉住「单一真源」的 lib 不变量（60 条，全名）

`semantics::cpu_isa::tests`（26）：

```
names_are_unique_after_normalization            lookup_normalizes_underscore_and_case
cloneable_set_matches_gcc13                     dispatch_order_matches_gcc_measured_chain
every_bit_has_a_cache_slot                      every_bit_is_covered_by_probe_sequence
probe_slots_are_within_model_size               slot_masks_aggregate_per_slot
subleaf_slots_are_distinct_from_subleaf_zero    probe_gates_are_satisfiable
host_reference_gates_subleaves                  host_reference_reads_extended_leaves
avx_family_needs_xcr0                           osxsave_gate_bit_is_the_table_entry
table_agrees_with_host_detector                 host_cpu_is_agrees_with_gcc13_measured_answers
cpu_is_names_equal_the_gcc13_measured_set       cpu_is_names_are_mutually_exclusive_and_imply_types
decode_formula_matches_gcc_for_each_vendor      class_ids_are_unique_and_derived
classify_every_id_is_addressable                classify_guards_exist_in_feature_table
classify_predicates_are_well_formed             classify_chain_semantics_on_synthetic_inputs
vendor_dwords_agree_with_host_leaf0             vendor_dwords_rebuild_the_vendor_strings
```

`link::target_clones::tests`（18）：

```
bit_table_is_the_shared_cpu_isa_source        dispatch_order_follows_cpu_isa_priority_not_declaration_order
linker_path_cloneable_bits_have_no_subleaf    emit_cpuid_leaf_caches_by_leaf_and_subleaf
osxsave_gate_jumps_away_when_clear            xsave_requirement
generate_resolver_blob_deterministic          generate_resolver_default_only
generate_resolver_sse42_and_default           resolver_code_contains_cpuid_for_avx2
resolver_patch_vaddrs                         parse_single_arch / parse_multi_arch_default / parse_uses_first_as_default
arch_to_features_default_empty / _sse42 / _avx2 / _unknown_empty
```

（其中 `bit_table_is_the_shared_cpu_isa_source` 就是「本模块不得自列 leaf/bit 常量」的字面钉法；
`linker_path_cloneable_bits_have_no_subleaf` 解释为什么内置链接器那条路径的位表**恰好**不需要
subleaf —— 见 §6.3。）

`link::ifunc::tests`（9）：

```
bind_static_exec_ifuncs_binds_to_default_variant   bind_static_exec_ifuncs_errors_without_default_variant
bind_static_exec_ifuncs_noop_without_ifunc         test_irelative_relas_count
test_register_and_lookup                           test_parse_dcc_ifunc_section_empty / _single / _roundtrip / _truncated
```

`codegen::x86_64::ifunc::tests`（7）见 §8.2 表。

### 8.7 被 filter 带出、但**不是本包写的**判据（20 条，只作不回归护栏）

`tests/integration/c_cross.rs`（本包未改）：`target_clones_resolver_code_generation`、
`target_clones_resolver_vaddr_patching`（内置链接器 `.text.ifunc` 那条路径，§6.3 说明它为何保留）、
`cpu_feature_cli_{apx,avx10_256,avx10_512,cet,default_none,mavx,mavx2_implies_avx,mavx512f_implies_avx,mavxvnni_implies_avx,mfma_implies_avx,mno_avx,mno_avxvnni}`、
`cpu_feature_verbose_output_shows_features`、`mcpu_cli_parsing`、
`percpu_{access_runtime,decl_compiles,full_support_compiles,offset_compiles}`。
全部 rc=0。列出来的理由：它们与本轮共用 `-march`/CPU 特性名解析，本包若改坏它们不会有人提醒。

### 8.8 判据覆盖不到的地方（不在这里留白）

| 点 | 现状 | 为什么没有判据 |
|----|------|----------------|
| 每个变体真的用不同 ISA 编译 | 变体机器码逐字节相同（§7.1 第 1 条） | 功能不存在，任何判据都会假绿；等 `ast.rs` 带上 per-function ISA 上下文后需要一条「`compute.avx` 里有 `vmovdqu`、`compute.default` 里没有」的判据 |
| AMD / Zhaoxin 机器上的 `__builtin_cpu_is` 型号名 | 本机只能验 Intel 那一支 | 判据设计成两层（合成输入 + 本机与 gcc 差分）已经最大化覆盖；真正的交叉验证需要换硬件，不是代码能补的 |
| `.o` 侧 `readelf -s` 直接显示 `IFUNC` 字样 | 显示 `<OS specific>: 10` | 写入点 `src/codegen/shared/elf.rs` 不在本包所有权（§7.2 第 1 条）；判据一律读原始 `st_info` 字节，不受 `EI_OSABI` 影响 |
| 静态（内置链接器）路径的运行时派发 | 绑 default + 播报 | 缺口在 `layout.rs`/`plt_got.rs`/`elf_exec.rs`（§7.1 第 2 条）；现有判据钉的是「不许静默」，不是「静态也派发」 |
| 大 `.init_array`/TLS/异常表与 resolver 的交互 | 未覆盖 | 本轮不涉及（resolver 不依赖 `.init`，§3.2）；如果以后有人把 `INIT_DONE` 守卫改成依赖 `.init_array`，§8.4 前四条会红 |

## 9. 改动落点（本包实际动过的文件；行号按本轮当前工作树复核）

工作树里带着其它在途包（`disasm.rs`/`ssa.rs`/`opt.rs`/`tyclass.rs`/`mem/access.rs` 等）的未
提交改动，所以**不能**用 `git diff --stat` 的总行数当本包体量。下表只列本包动过的点，
并且已逐个核过：那些不列在这里的文件本包一行未改（已验证：`instructions.rs`、
`abi/tyclass.rs`、`emit/functions.rs`、`mem/access.rs`、`parallel/orchestration.rs`、
`shared/elf.rs` 的 diff 里与 ifunc/cpu 相关的 `+/-` 行数均为 **0**）。

| 文件 | 本包动了什么 | 锚点（行号） | 钉它的判据 |
|------|--------------|--------------|------------|
| `src/semantics.rs` | **新增** `pub mod cpu_isa`（唯一真源）：`IsaFeature`/`FEATURES`、`CpuidBit`（带 subleaf）、`PROBES`（9 次探测）、`SLOT_*`+`SLOT_WORDS`+`MODEL_BYTES=100`、`cache_slot`/`probe_covers`/`host_supported`、`dispatch_priority`/`dispatch_order`、`CLASSIFY`+`decode_family_model`/`vendor_kind_of_bx`/`type_id`/`subtype_id`/`cpu_is_holds`、`CPU_IS_NAMES`、`host_processor_class`；尾部 `mod tests`（26 条） | `:768`–`:2985`（`FEATURES:930`、`cpuid_bits:1175`、`xcr0_mask:1180`、`dispatch_priority:1190`、`SLOT_*:1214-1252`、`PROBES:1290`、`cache_slot:1431`、`probe_covers:1462`、`VENDOR_SIGS:1551`、`host_supported:1586`、`CLASSIFY:1735`、`CPU_IS_NAMES:2063`、`classify:2144`、`mod tests:2236`） | §8.6 那 26 条 + `attr_target_clones_resolver_bits_come_from_cpu_isa_table` + 两支矩阵用例 |
| `src/codegen/x86_64/ifunc.rs` | **新增文件（1818 行）**：AST 展开 `expand_target_clones`、DCE 保留 `retain_ifunc_resolvers`、合成器 `ensure_cpu_model`/`emit_cpu_indicator_init`/`emit_resolver_body`/`emit_ifunc_synthesis`/`render_ifunc_synth_asm`、builtin 真实现 `emit_cpu_supports`/`emit_cpu_is`/`emit_cpu_init_builtin`、共用发射器 `emit_feature_check`、诊断码 E1125/E1126/E1127；尾部 `mod tests`（7 条） | 模块头 `:1-20`；`CPU_MODEL_NAME:69`、`CPU_INIT_NAME:71`、`mangle_variant_sym:82`、`collect_clone_bases:169`、`expand_target_clones:266`、`retain_ifunc_resolvers:460`、`ensure_cpu_model:519`、`emit_cpu_indicator_init:563`、`emit_resolver_body:950`、`emit_ifunc_synthesis:1049`、`emit_cpu_init_builtin:1154`、`emit_cpu_supports:1169`、`emit_cpu_is:1236`、`emit_feature_check:1377`、RIP 形式表 `:1461-1503` | §8.2 全部 + §8.3 前三条 + §8.4 前三条 |
| `src/codegen/x86_64/mod.rs` | 挂新模块 | `:34 pub mod ifunc;` | 编译期 |
| `src/codegen/x86_64/codegen.rs` | `Codegen` 新增四个字段（`ifunc_synth`/`ifunc_defs`/`cpu_builtin_used`/`cpu_model_registered`）+ `use …::ifunc` | `:322`、`:324`、`:327`、`:330`、初始化 `:733-734` | `attr_target_clones_ifunc_symbols`、`cpu_builtin_supports_runs_under_builtin_linker_static` |
| `src/codegen/x86_64/elf/rel_symbols.rs` | symtab 侧：基名不再发 UND；`ifunc_defs` 循环保 `ty: STT_GNU_IFUNC` 且 `st_value`/`st_size` 取自自己的 resolver；`__attribute__((ifunc("r")))` 同形；无体时报错而不静默同址 | `:213`（注释）、`:434`、`:554`、`:630`、`:683` | `attr_target_clones_ifunc_symbols`、`m27_attr_ifunc_symbol_type_is_ifunc` |
| `src/codegen/x86_64/elf/output.rs` | `.text` 尾部拼接合成体（两个出口都调）+ 变体/基名的命名口径与 `rel_symbols.rs` 对齐 | `:114`、`:282`、`:331` | 同上 + §8.2 后四条 |
| `src/codegen/x86_64/special/builtins.rs` | 三个 `__builtin_cpu_*` 从恒假桩改为转发真实现（带缺参分支）；`.bss`→`.data` 的注释改正（补 NOBITS 理由与判据名） | `:2506`（`__builtin_cpu_init`）、`:2510`（`supports`）、`:2520`（`is`），缺参 `:2515`/`:2525` | §8.4 全部 |
| `src/parser.rs` | 把三个 builtin 列入 W1015「返回类型未知」的白名单（它们是编译器内建，没有声明可写） | `:11476-11478` | `builtin_cpu_compiles` |
| `src/driver.rs` | （一）在 `ir::lower` **之前**调 `expand_target_clones`（仅 x86_64）；（二）**删除**「.o 含 IFUNC 就改道内置链接器」分支与 `elf_has_ifunc_symbols` 整个函数；（三）宿主链成功后报 `dcc: <out> (via host cc)`；（四）内置链两条路径统一自报 `dynamic/static (no PT_INTERP / DT_NEEDED), N bytes` | 展开调用 `:686`（上方 `:682-683` 就是「改道分支不再需要」的注释）；删除说明 `:3092-3095`；`via host cc` `:3087`；链接方式 `:2905-2913` | §8.5 全部 |
| `src/link/target_clones.rs` | 位表改为 `cpu_isa` 的**薄转发**（`arch_to_cpuid_bits`/xcr0 门控不再自列常量）；新增 `CpuidBit` 的 subleaf 支持；模块头写清「为什么两条路径都要留着」（`:19-40`）；新增钉字段的不变量测试 | `:19-40`（保留理由）、`:76-77`（单一真源声明）、`:84`（`arch_to_cpuid_bits`）、`:103`（subleaf）、`:568`（钉字测试） | §8.6 那 18 条 |
| `src/link/ifunc.rs` | 静态路径的 IFUNC 绑定器（绑 `<base>.default`、无 default 则错）+ 逐项缺口清单；`render_ifunc_synth_asm` 与 `.dcc.ifunc` 解析保留 | `bind_static_exec_ifuncs:162`；模块头缺口清单 | §8.6 那 9 条 + `target_clones_builtin_linker_static_binds_default_and_announces` |
| `src/link/mod.rs` | 静态绑定后的**无条件**播报（说清 CPUID 派发在此路径被禁用 + 怎么拿到会派发的产物） | `:550-575`（播报串 `:567`） | 同上 |
| `tests/integration/c_attr_builtin.rs` | 三态哨兵翻成断言动态；新增 §8.2-§8.5 里属于本文件的那批判据；`assert_dynamically_linked` 三项 helper | helper `:433-454`；翻转后的判据 `:456` 起 | §8.2-§8.5 |
| `tests/integration/c_link_debug.rs` | `ifunc_resolver_runtime` 取消 skip 出口 + 改断动态；链接方式播报判据改写 | 两个函数体 | §8.3、§8.5 |
| `tests/fixtures/c/attr_target_clones_{select,selected,ifunc_behavior,cpu_builtin,cpu_matrix,cpu_is_matrix}.c` | **新增 6 支夹具**（依次服务：判定链顺序 / 选型可观察 / 行为级选型 / builtin 四不变量+自包含 / 60 名字 supports 矩阵 / 51 型号名矩阵） | — | §8.2-§8.4 |
| `tests/fixtures/c/attr_target_clones.c` | 既有基准夹具（未改） | — | `attr_target_clones_compile_and_run` 等 |
