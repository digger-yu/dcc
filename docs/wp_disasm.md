# `-S` 反汇编读数收尾：四处文本形状缺陷、常驻判据、上限归零与错位探测

范围：`src/codegen/disasm.rs`（只用于自家 `-S` 读数与 `rex_matrix` 判据，不进产物）、
`scripts/check_disasm_coverage.sh`、`Makefile` 的 `disasm-cov` 目标（只动 `CEILING` 与
该目标自己的注释）。本文里所有 objdump/GAS 读数都是命令输出**原文**（含 objdump 的
两空格对齐、`(bad)` 字样、GAS 的 `Warning:`），钉法见附录 A；没有一处是凭 ISA 手册记忆写的。

## 0. 结论一览

两套口径，都在最终二进制（`CARGO_TARGET_DIR=.work_tmp/tgt_seg` 产物，rustfmt 归位后
重编于 00:20）上实测；下表与 §4.2/§5.1/§5.4 的数字全部用同一二进制重跑过（门 exit=0）：

- **门**（`scripts/check_disasm_coverage.sh`，`find` 递归 346 个夹具 × `-O0/-O2`）：
  指令行分母 **278270**（-O0 137782 + -O2 140488），`# UNKNOWN` **0** 条，
  错位探测 **2470** 个函数参与比较 / **16** 个不一致 / **25** 个边界单边。
- **形状与回炸扫描**（顶层 glob 317 个夹具 × `-O0/-O2`，可编译的「夹具×档位」**626** 个，
  制表符开头的行 **258714** 行（其中纯指令行 246839，其余是 `.cfi_*` / `.p2align` 等
  制表符缩进的伪指令）：

| 读数形状 | 修前（`target/release/dcc`，19:01） | 修后 |
| --- | --- | --- |
| `# UNKNOWN`（`disasm-cov` 的指标） | 0 | **0** |
| `f???` / `fi???` 占位符 | **1013** 行 | **0** |
| 假助记词 `shift`（REX 路径漏 rcl/rcr） | **5** 行 | **0** |
| SIB 变址掉在左括号外（`,%rcx,4(%r11)`） | **75** 行 | **0** |
| 间接 call/jmp 少 AT&T 的 `*` | **24** 行（其中 22 行目标还取成 32 位名） | **0**（24 行全部带星、全部 64 位名） |
| `push` 错带 `*` / `push` 目标取 32 位名 | 0 / 0 | 0 / 0 |
| `-S` 文本被 `as` 整体接受 | **557 / 626** | **612 / 626** |
| `as` 首条错误（按夹具×档位计） | 69 | 14 |
| `Makefile` 的 `disasm-cov` `CEILING` | 13 | **0**（实测为 0 才写 0） |
| lib 判据 `disasm` / `rex_matrix` | — | **10 passed / 15 passed** |

一句话结论：`# UNKNOWN` 这条指标本身已经到 0，但它有**三个盲区**——错位后读出来的
**有名字的假指令**、**有名无实的占位符**（`f???`、`shift`、`ff`）、**语法非法或变义的
文本**（SIB 变址在括号外、间接转移少 `*`、无尺寸字母的立即数入内存）。归零靠的四处
根因里，ALU `/r` 表与 MOVSXD 的 REX.B 双重施加是主控那轮改的（本文只补了常驻判据）；
x87 576 格真名、shift/rcl-rcr 表、`decode_mem` 的 SIB 拼法、`FE`/`FF` 间接转移拼法
四处是本轮改的。第一类盲区（错位）本身不是解码器造成的，所以给指标补了一条正交的
每函数指令条数探测器（当前只报告、未进断言，实测与理由在 §5.3）；
「无尺寸字母」那一条仍开着（§6.2）。

---

## 1. 任务 1：SIB 带变址时 AT&T 文本拼法是坏的

### 1.1 objdump 真值（原文）

`as -o sib_final.o sib_final.s` + `objdump -d --no-show-raw-insn sib_final.o`，
源码是逐行 `.byte`（`49 63 04 8b` / `4c 8d 04 8b` / `49 63 44 8b f8` /
`49 63 84 8b 78 56 34 12` / `65 49 63 04 8b` / `48 8b 04 0d 40 00 00 00` / `c3`）：

```text
0000000000000000 <sibf>:
   0:	movslq (%r11,%rcx,4),%rax
   4:	lea    (%rbx,%rcx,4),%r8
   8:	movslq -0x8(%r11,%rcx,4),%rax
   d:	movslq 0x12345678(%r11,%rcx,4),%rax
  15:	movslq %gs:(%r11,%rcx,4),%rax
  1a:	mov    0x40(,%rcx,1),%rax
  22:	ret
```

另外两条同族真值（`49 63 04 cb` scale=8、`4c 8d 64 24 10` SIB 无变址）：

```text
   0:	movslq (%r11,%rcx,8),%rax
   4:	lea    0x10(%rsp),%r12
```

要点：形状是 `[段前缀][disp/符号](base[,index[,scale]])` —— **base/index/scale 全在左括号
之内**；无基址时 base 那一格是空的（`0x40(,%rcx,1)`），此时位移**就是地址本身**，为 0
也必须写出来；`%gs:` 贴在左括号**外**。

### 1.2 根因与复现

`Dec::decode_mem` 原先把 `,{index},{scale}` 追加到 `inner`（disp/符号那一串）末尾，
最后才 `format!("{inner}({base})")`，于是变址整段掉到左括号**外面**：

```text
movsxd ,%rcx,4(%r11), %rax          # 正确应为 movsxd (%r11,%rcx,4), %rax
movsxd -8,%rcx,4(%r11), %rax        # 正确应为 movsxd -8(%r11,%rcx,4), %rax
```

复现（同一批源码，新旧二进制逐行计数，正则 `grep -cP ',%[a-z0-9]+,[0-9]+\('`）：

| 夹具 | 档位 | 修前行数与原文 | 修后 |
| --- | --- | --- | --- |
| 全语料合计 | -O0/-O2 | **75 行** | **0 行** |
| `static_local_arrays.c` | -O0 / -O2 | 2 / 2：`movsxd ,%rcx,4(%r11), %rax` | `movsxd (%r11,%rcx,4), %rax` |
| `sched_rq_lock_patterns.c` | -O0 | 4：`mov ,%rcx,1(%rdi), %esp`、`movsxd ,%rcx,2(%rcx), %rsp` | `mov (%rdi,%rcx,1), %esp`、`movsxd (%rcx,%rcx,2), %rsp` |
| `sched_rq_lock_patterns.c` | -O2 | 3：同上形状 | 同上 |

这条**长度是对的**，所以 `# UNKNOWN` 一条都不报（修前全语料 UNKNOWN 也已经是 0）；
它是「文本非法」而非「读不出来」，属于指标盲区。GAS 对它的报错是
`Error: expecting operand before ','; got nothing` —— 修前 69 个失败组合里**这一类占 23 个**
（见 §5.4）。

### 1.3 修法与不退化的三件事

现在 `decode_mem` 先算「括号外那一截」（段前缀 + 符号名或位移），再把
`(base[,index[,scale]])` 整段拼在后面；`parens = base.is_some() || !index.is_empty()`
保证无基址 + 无变址的裸绝对形式（`mov 0x0,%rax`）仍然不加括号。三条既有约定实测未退化，
并已进常驻判据（§2 的 `decodes_movsxd_sib_and_keeps_stream_aligned`）：

- RIP 相对符号名：`48 8b 05 <disp32>` + data 表命名 ⇒ `mov arr(%rip), %rax`
- 段前缀贴在内存操作数上：`65 49 63 04 8b` ⇒ `movsxd %gs:(%r11,%rcx,4), %rax`
  （objdump 同形：`movslq %gs:(%r11,%rcx,4),%rax`）
- `%rip` 形式与 SIB `base=101` 的「无基址」不是同一回事（后者不得贴 `%rip`，
  真值 `mov 0x40(,%rcx,1),%rax`）

顺带：`decode_x87_mem` 曾是 `decode_mem` 之外的**第二份括号拼法**（丢 SIB 变址、把
mod=0/rm=5 硬编成 `0(%rip)` 且不查符号名、忽略 REX.B、整个吞掉段前缀），本轮已让
内存操作数只有 `decode_mem` 一份事实源，x87 判据里钉了 `db 04 8b`→`fildl (%rbx,%rcx,4)`
与 `64 dd 00`→`fldl %fs:(%rax)` 作为「没被绕过」的证明。

### 1.4 「-S 可被 as 回炸」的门：存在，但对这些形状是漏网之鱼

门是 [`s_output_roundtrips_through_gas_for_small_vector`](../tests/integration/c_asm.rs)
（`tests/integration/c_asm.rs:665`），只覆盖**一个**夹具 `tests/fixtures/c/vec_small_return.c`。
实测该夹具的 `-S` 文本（`-O2`）：间接转移 **0** 行、SIB 带变址 **0** 行、x87 **0** 行
⇒ 这条门在结构上**不可能**抓到本轮任何一个缺陷。按任务书要求没有新建门；把「全语料
回炸」的实测结果放在 §5.4，作为待决策项（要不要把这条门从 1 个夹具扩到全语料）。

---

## 2. 任务 2：常驻判据（`src/codegen/disasm.rs` 的 `mod tests`）

沿用既有模式：**每段字节后面都接一条 `0xc3`，既要念对名字、也要能把后面那条 `ret`
读回来**（首行整行相等 + 末行必须是 `\tret`，整行比较而不是 `contains`）。

| 判据 | 覆盖 | 实跑 |
| --- | --- | --- |
| `decodes_alu_adc_sbb_forms_and_keeps_following_stream_aligned` | 10 例：`4d 11 d8`→`adc %r11, %r8`、`4d 19 df`→`sbb %r11, %r15`、`4d 13 d8`→`adc %r8, %r11`、`41 11 d8`→`adc %ebx, %r8d`、`11 c8`→`adc %ecx, %eax`、`10 c8`→`adc %cl, %al`、`12 c8`→`adc %al, %cl`、`13 c8`→`adc %eax, %ecx`、`39 c8`→`cmp %ecx, %eax`、`85 c8`→`test %ecx, %eax` | ok |
| `decodes_movsxd_sib_and_keeps_stream_aligned` | 8 例（§1.1 全部真值：`(%r11,%rcx,4)`、`(%rbx,%rcx,4)`、`-8(...)`、disp32 `305419896(...)`、scale=8、`%gs:`、无基址 `64(,%rcx,1), %rax`、SIB 无变址 `16(%rsp), %r12`）；两条反向对照：整段无 `UNKNOWN`、无「逗号开头的操作数」；另含 RIP 符号名不退化 | ok |
| `decodes_x87_forms_from_objdump_table_and_keeps_stream_aligned` | 抽样约 30 例（内存形与寄存器形，含 `db 04 8b`→`fildl (%rbx,%rcx,4)`、`64 dd 00`→`fldl %fs:(%rax)`、`df e0`→`fnstsw %ax`、`db e0`→`fneni`） | ok |
| `decodes_all_x87_space_against_objdump_sweep` | **576 格全表**：mod=3 的 512 格（按 64 个 `(opcode,reg)` 组，四张表不重不漏，判据里 `assert_eq!(groups, 64)` / `assert_eq!(cells, 512+64)`）+ 内存形 64 格；每组后面接 `0xc3` 并要求末行 `\tret`；全局反查 `???` | ok |
| `decodes_shift_rotate_rcl_rcr_forms_and_keeps_following_stream_aligned` | 15 例（rcl/rcr 四种宽度形 + 其余六格不退化，`c0 38 02`→`sar $2, (%rax)`）；反查 `UNKNOWN` 与假助记词 `shift` | ok |
| `decodes_indirect_branch_and_push_and_keeps_following_stream_aligned` | 20 例（§4 的 call/jmp/push/inc/dec 真值 + 4 格未编码 `/digit` 报 UNKNOWN）；反查少星、32 位目标名、假助记词 `ff` | ok |

实跑（`nice -n 19 cargo test --release -j1 --lib -- --test-threads=1 …`）；
上表 6 条是本轮/上轮新增，另 4 条是既有的 `decodes_mov_imm_and_ret`、
`decodes_string_instructions_and_keeps_following_stream_aligned`（本轮新增判据的样板）、
`decodes_group_ae_by_modrm_reg_field`、`decodes_lock_cmpxchg_and_movsd`，共 10 条：

```text
test result: ok. 10 passed; 0 failed; … 2006 filtered out   # disasm
test result: ok. 15 passed; 0 failed; … 2001 filtered out   # rex_matrix
```

### 2.1 与本文件惯例（而非 objdump 逐字）不同的三处

- 本文件的打印惯例是「助记符后一个空格、逗号后一个空格、不带尺寸字母、位移用十进制」，
  所以断言文本是 `adc %ecx, %eax`（objdump 紧凑写法 `adc %ecx,%eax`）、
  `mov 64(,%rcx,1), %rax`（objdump 十六进制 `0x40(,%rcx,1)`）；而 `movslq` 在这里叫
  `movsxd`。**星号、寄存器名、两侧顺序则逐字照抄 objdump。**
- 任务书给的 `12 c8`→`adc %eax, %ecx` 与真值不符。钉出来的原文是
  （`objdump -d alu.o`，逐字节 `12 c8` / `13 c8` / `11 c8` / `10 c8` / `39 c8` / `85 c8`）：

  ```text
     0:	adc    %al,%cl        # 12 c8  （ADC r8, r/m8：reg=cl 是目的）
     2:	adc    %eax,%ecx      # 13 c8  （ADC r32, r/m32）
     4:	adc    %ecx,%eax      # 11 c8  （ADC r/m32, r32）
     6:	adc    %cl,%al        # 10 c8  （ADC r/m8, r8）
     8:	cmp    %ecx,%eax      # 39 c8
     a:	test   %ecx,%eax      # 85 c8
  ```

  即 `12`/`13` 是把 reg 字段当**目的**（`%al`/`%eax` ← `adc` 自 `%cl`/`%ecx`），
  判据按真值同时钉了 `12 c8`→`adc %al, %cl` 与 `13 c8`→`adc %eax, %ecx` 两格。
- 本文件把「`d9 c8` 之类 objdump 会念 `fxch %st(0)`」的格也照抄 `fxch`，不改成 `nop`
  （objdump 原文：`r_d9_c8>: fxch %st(0)`）。

---

## 3. 顺带钉全的 x87 D8..DF 全空间（占位符 `f???`，修前 1013 行）

旧实现只认十几条，其余一律打成 `f???` / `fi???`；`as` 直接拒绝这种 token
（`Error: invalid character '?' in mnemonic`，修前 69 个失败组合里占 **14** 个），
而它**有名字**，所以 `# UNKNOWN` 一条都不报。修前后实测：

| 形状 | 修前 | 修后 |
| --- | --- | --- |
| 含 `???` 的读数行 | **1013** 行（18 个夹具×档位） | **0** |
| `fenv.c -O0` 的 `-S` 文本被 `as` 接受 | 否（同上错误） | 是 |
| `ld80_arith.c`（-O0/-O2） | 否（181 / 201 条 `???`） | 是（0 条） |

### 3.1 内存形（mod≠3，只看 `(opcode, reg)`）64 格全表原文

`as` 组 64 个「`.byte op; .byte reg<<3; .byte 0xc3`」的小函数后 `objdump -d` 的读数
（每格一个小函数，标签 `m_<opcode>_<reg>` 就是那一格；钉法见附录 A）。排版说明：objdump
本来是「标签行 + 指令行 + `ret` 行」三行一格，下面把**标签行与首条指令行并成一行**，
列间空白按并排需要收拢过；**指令列本身逐字未改**（含 objdump 的两空格对齐与 `(bad)`），
附录 A 给了产生这张表的 join 命令，逐列对过是 64/64 一致。

```text
0000000000000000 m_d8_0>:    0:	fadds  (%rax)
0000000000000003 m_d8_1>:    3:	fmuls  (%rax)
0000000000000006 m_d8_2>:    6:	fcoms  (%rax)
0000000000000009 m_d8_3>:    9:	fcomps (%rax)
000000000000000c m_d8_4>:    c:	fsubs  (%rax)
000000000000000f m_d8_5>:    f:	fsubrs (%rax)
0000000000000012 m_d8_6>:   12:	fdivs  (%rax)
0000000000000015 m_d8_7>:   15:	fdivrs (%rax)
0000000000000018 m_d9_0>:   18:	flds   (%rax)
000000000000001b m_d9_1>:   1b:	(bad)  (%rax)
000000000000001e m_d9_2>:   1e:	fsts   (%rax)
0000000000000021 m_d9_3>:   21:	fstps  (%rax)
0000000000000024 m_d9_4>:   24:	fldenv (%rax)
0000000000000027 m_d9_5>:   27:	fldcw  (%rax)
000000000000002a m_d9_6>:   2a:	fnstenv (%rax)
000000000000002d m_d9_7>:   2d:	fnstcw (%rax)
0000000000000030 m_da_0>:   30:	fiaddl (%rax)
0000000000000033 m_da_1>:   33:	fimull (%rax)
0000000000000036 m_da_2>:   36:	ficoml (%rax)
0000000000000039 m_da_3>:   39:	ficompl (%rax)
000000000000003c m_da_4>:   3c:	fisubl (%rax)
000000000000003f m_da_5>:   3f:	fisubrl (%rax)
0000000000000042 m_da_6>:   42:	fidivl (%rax)
0000000000000045 m_da_7>:   45:	fidivrl (%rax)
0000000000000048 m_db_0>:   48:	fildl  (%rax)
000000000000004b m_db_1>:   4b:	fisttpl (%rax)
000000000000004e m_db_2>:   4e:	fistl  (%rax)
0000000000000051 m_db_3>:   51:	fistpl (%rax)
0000000000000054 m_db_4>:   54:	(bad)  (%rax)
0000000000000057 m_db_5>:   57:	fldt   (%rax)
000000000000005a m_db_6>:   5a:	(bad)  (%rax)
000000000000005d m_db_7>:   5d:	fstpt  (%rax)
0000000000000060 m_dc_0>:   60:	faddl  (%rax)
0000000000000063 m_dc_1>:   63:	fmull  (%rax)
0000000000000066 m_dc_2>:   66:	fcoml  (%rax)
0000000000000069 m_dc_3>:   69:	fcompl (%rax)
000000000000006c m_dc_4>:   6c:	fsubl  (%rax)
000000000000006f m_dc_5>:   6f:	fsubrl (%rax)
0000000000000072 m_dc_6>:   72:	fdivl  (%rax)
0000000000000075 m_dc_7>:   75:	fdivrl (%rax)
0000000000000078 m_dd_0>:   78:	fldl   (%rax)
000000000000007b m_dd_1>:   7b:	fisttpll (%rax)
000000000000007e m_dd_2>:   7e:	fstl   (%rax)
0000000000000081 m_dd_3>:   81:	fstpl  (%rax)
0000000000000084 m_dd_4>:   84:	frstor (%rax)
0000000000000087 m_dd_5>:   87:	(bad)  (%rax)
000000000000008a m_dd_6>:   8a:	fnsave (%rax)
000000000000008d m_dd_7>:   8d:	fnstsw (%rax)
0000000000000090 m_de_0>:   90:	fiadds (%rax)
0000000000000093 m_de_1>:   93:	fimuls (%rax)
0000000000000096 m_de_2>:   96:	ficoms (%rax)
0000000000000099 m_de_3>:   99:	ficomps (%rax)
000000000000009c m_de_4>:   9c:	fisubs (%rax)
000000000000009f m_de_5>:   9f:	fisubrs (%rax)
00000000000000a2 m_de_6>:   a2:	fidivs (%rax)
00000000000000a5 m_de_7>:   a5:	fidivrs (%rax)
00000000000000a8 m_df_0>:   a8:	filds  (%rax)
00000000000000ab m_df_1>:   ab:	fisttps (%rax)
00000000000000ae m_df_2>:   ae:	fists  (%rax)
00000000000000b1 m_df_3>:   b1:	fistps (%rax)
00000000000000b4 m_df_4>:   b4:	fbld   (%rax)
00000000000000b7 m_df_5>:   b7:	fildll (%rax)
00000000000000ba m_df_6>:   ba:	fbstp  (%rax)
00000000000000bd m_df_7>:   bd:	fistpll (%rax)
```

（完整命令与 `.byte` 生成式见附录 A；判据 `decodes_all_x87_space_against_objdump_sweep`
里的 `MEM` 表就是这 64 格的逐字对账。）
**4 格 objdump 读 `(bad)`**：`D9 /1`、`DB /4`、`DB /6`、`DD /5` —— 实现按兜底出口报
`# UNKNOWN`，不猜名字。

### 3.2 寄存器形（mod=3，看 `(opcode, reg, rm)`）512 格

同一钉法（`.byte op; .byte 0xC0|rm; .byte 0xc3`）逐格读出后对账：**155 格 objdump 读
`(bad)`**（按 opcode 分布 D8 0、D9 20、DA 31、DB 10、DC 16、DD 24、DE 15、DF 39），
其余 357 格有名有姓。边角摘录（`objdump -d --no-show-raw-insn regx87.o` 的读数，**每行
并排了两格**，指令文本逐字未改；地址就是那一格的 objdump 偏移）：

```text
  0:	fadd   %st(0),%st         3:	fadd   %st,%st(0)
  6:	ffree  %st(0)             9:	fstp   %st(0)
  c:	fcompp                   12:	fnstsw %ax
 15:	fneni(8087 only)          18:	fndisi(8087 only)
 1b:	fnsetpm(287 only)         1e:	frstpm(287 only)
 21:	fldpi                      24:	fsin
 27:	fadd   %st(1),%st         2a:	fxch   %st(1)
 2d:	fcmovb %st(1),%st         30:	fucomi %st(1),%st
 33:	fucomip %st(1),%st        36:	faddp  %st,%st(1)
 39:	fadd   %st,%st(1)         3c:	fchs
 48:	fst    %st(0)             4b:	(bad)
```

一个只有靠原文才能发现的坑：`objdump` 在 `db e0/e4/e5` 这类格子上打的是**带注记的**
名字（`fneni(8087 only)`、`fnsetpm(287 only)`），而 `as` 只认裸名 —— 实测 `fneni`
组装回 `db e0`、`frstpm` 组装回 `db e5`。所以表里记裸名，注记只在注释里留痕，
否则 `-S` 文本自己就回炸不了。

---

## 4. 任务 1 顺手发现的第四个缺陷：`FE`/`FF` 组的间接转移文本

### 4.1 objdump 真值（原文）

`ind.s` 依次是 `ff d0`、`ff e0`、`ff 10`、`ff 20`、`41 ff d2`、`41 ff e2`、
`ff 90 78 56 34 12`、`ff 14 8b`、`66 ff d0`、`ff 30`、`ff f0`、`ff d8`、`ff e8`、
`ff f8`、`c3`；`objdump -d --no-show-raw-insn ind.o` 的读数原文：

```text
0000000000000000 <ind>:
   0:	call   *%rax
   2:	jmp    *%rax
   4:	call   *(%rax)
   6:	jmp    *(%rax)
   8:	call   *%r10
   b:	jmp    *%r10
   e:	call   *0x12345678(%rax)
  14:	call   *(%rbx,%rcx,4)
  17:	call   *%ax
  1a:	push   (%rax)
  1c:	push   %rax
  1e:	lcall  (bad)
```

未编码格与 `push`/`pop`/TSX 一族（`pin2.s` 里**每格一个函数**，标签名就是那串字节，
所以读数与字节一一对得上）：上面那份 `ind.o` 只抄到 `1e:` 为止 —— `ff d8`、`ff e8`、
`ff f8` 三格连写时 objdump 在 `1e: lcall (bad)` 之后是按字节再同步的（它把 `d8 ff` 读成
`fdivr %st(7),%st`、把 `e8` 打成 `.byte 0xe8`），所以**未编码格以下面每格一个函数的
`pin2.o` 为权威读数**：

```text
0000000000000000 <fe_e0>:
   0:	(bad)
0000000000000006 <fe_f8>:
   6:	(bad)
0000000000000009 <ff_f8>:
   9:	(bad)
000000000000000c <ff_d8>:
   c:	lcall  (bad)
000000000000000f <ff_e8>:
   f:	ljmp   (bad)
0000000000000012 <ff_c8>:
  12:	dec    %eax
0000000000000015 <ff_f0>:
  15:	push   %rax
0000000000000018 <ff_30>:
  18:	push   (%rax)
000000000000001b <pop_00>:
  1b:	pop    (%rax)
0000000000000021 <xc6_f8>:
  21:	xabort $0x1
0000000000000025 <xc7_f8>:
  25:	xbegin 2c <xc7_f8+0x7>
```

（同族还有 `fe c0`→`inc %al`、`fec8`→`dec %al`、`ff c0`→`inc %eax`、
`48 ff c0`→`inc %rax`、`41 ff c0`→`inc %r8d`、`66 ff f0`→`push %ax`、
`48 ff f0`→`rex.W push %rax`、`ff 91 08 00 00 00`→`call *0x8(%rcx)`，都是
`fe.o` / `pushpin.o` / `incpin.o` 的原文读数。最后三组特别有用：它们说明
`inc`/`dec`（`/0`、`/1`）的名字**跟**宽度与 REX.W，而 `call`/`jmp`/`push`
（`/2`、`/4`、`/6`）的目标名**不跟** —— 旧实现把两族共用一个 `width` 就同时错了两边。）

三条只能靠原文钉死的规矩：**(1)** 间接 call/jmp 必须带 `*`，`push` 不带；
**(2)** mod=3 的目标名**恒 64 位**，不跟 REX.W、也不跟 `0x66`（`0x66` 只能把它压到
16 位：`66 ff d0` → `call *%ax`），旧实现按 `width` 命名于是打出 `call %r10d`；
**(3)** `fe /2../7`、`ff /3`、`ff /5`、`ff /7` 在 64 位模式没有编码，objdump 读
`(bad)` / `lcall (bad)` / `ljmp (bad)` ⇒ 本文件按兜底出口报 `# UNKNOWN`，不打假名字。

### 4.2 复现与修后

全语料逐行计数（`ind_sweep2.sh`，awk 按字段判定，直接调用 `call sym` 不带星是**对的**，
不能算进坏形状）：

| 形状 | 修前（19:01 二进制） | 修后 |
| --- | --- | --- |
| `\tcall` / `\tjmp` 行总数 | 6393 | 6564 |
| 间接目标**少星**（`call %r10d`、`jmp (%rdi)`） | **24** | **0** |
| 目标取成 **32 位名**（`%r10d`/`%r11d`） | **22** | **0** |
| `push` 错带星 / `push` 目标 32 位名 | 0 / 0 | 0 / 0 |
| 假助记词 `\tff ` / `\tfe ` | 0 | 0 |

取样（同一份源码 `apply_ret.c -O0`、`builtin_misc.c -O0`）：
`call %r10d` → `call *%r10`；`jmp (%rdi)` → `jmp *(%rdi)`。
GAS 对旧文本的报错是 `Error: operand size mismatch for 'call'`（修前 69 个失败组合里
这一类占 **15** 个）。修法：`FE`/`FF` 两条路径（REX 与 legacy）收敛到同一张
`Dec::group_fe_ff_name(op, reg)` 表，`/digit` 不在表里就 `UNKNOWN`；目标名交给
`Dec::indirect_operand`（只在 `/2`、`/4` 前贴 `*`，mod=3 时按 `o16 ? 2 : 8` 命名）。

---

## 5. 任务 3：上限砍到实测值 + 错位探测器

### 5.1 实测与 `CEILING`

跑过两遍（数字一字不差）：上一轮的 23:31 产物与本轮 `rustfmt` 归位后重编的 00:20 产物；
命令与输出取后一份（`.work_tmp/seg_probe/gate_after_fmt.log`，exit=0）：

```sh
DCC=/root/dcc/target/release/dcc DCC_REPO=/root/dcc LEVELS="0 2" JOBS=2 \
  bash scripts/check_disasm_coverage.sh      # 实为 .work_tmp/tgt_seg/release/dcc，nice -n 19
```

```text
档位 -O0: 指令行 137782, 读不出 0
档位 -O2: 指令行 140488, 读不出 0
=== 合计: 指令行 278270, 读不出 0, 占比 0.0000%
=== 错位探测: 每函数条数比较 2470 个, 不一致 16 个, 边界单边 25 个
check_disasm_coverage: OK（0.0000%，上限 0）
```

`Makefile` 的 `disasm-cov` 由 `CEILING=13` 改为 **`CEILING=0`**（实测为 0 才写 0；
此后**任何一条** UNKNOWN 都会红），该目标注释里的历史行同步成
`235 → 26 → 13 → 0` 并列出降到 0 所靠的四处根因（ALU 表 / MOVSXD 的 REX.B 双重施加 /
x87 576 格 / shift 表）与两个计数盲区。没有动本目标以外的任何一行
（`git diff --stat -- Makefile` 只有 disasm-cov 块内的 3 个 hunk）。

### 5.2 探测器的成本与实现

对每个「夹具×档位」再跑一次 `dcc -c` + `objdump -d --no-show-raw-insn`，把两边的
**每函数指令条数**做表连接（`od.awk` / `dc.awk` / `sk.awk`）：两侧都只数指令行、
都跳过 `nop`（objdump 的对齐填充与 `-S` 里的显式 nop 都会假报 skew），`+` 号不切
函数名，遇到 `Disassembly of section` 重置当前函数。`-c` 编不过的夹具记 0 并跳过
（与 `-S` 编不过是同一条约定：不是解码器的锅，不得污染 skew 读数）。
实测全门耗时 **约 4 分钟**（JOBS=2，nice -n 19），比原门多一倍左右。

### 5.3 当前 skew 实测：**16 个函数不一致**（未进断言）

全部 16 条（`dcc -S` 读出的条数 vs `dcc -c` 对象里 objdump 数的条数）：

```text
O2 static_agg_by_value.c     main                 dcc=593 obj=625 差=32
O2 builtin_misc.c            test_jmp             dcc=44  obj=16  差=28
O0 builtin_misc.c            test_jmp             dcc=42  obj=16  差=26
O2 builtin_static_chain.c    test_static_chain    dcc=33  obj=50  差=17
O2 static_local_arrays.c     test_runtime_index   dcc=11  obj=23  差=12
O2 static_local_arrays.c     test_store           dcc=22  obj=32  差=10
O2 ssa_pipe_nested.c         nest_mixed           dcc=101 obj=109 差=8
O2 static_local_arrays.c     test_addr            dcc=16  obj=24  差=8
O2 static_local_arrays.c     test_const_index     dcc=45  obj=53  差=8
O0 sched_rq_lock_patterns.c  raw_spin_lock        dcc=26  obj=24  差=2
O2 sched_rq_lock_patterns.c  raw_spin_lock        dcc=27  obj=25  差=2
O2 seg_qualifier.c           make_gs_ptr          dcc=7   obj=9   差=2
O2 seg_qualifier.c           ret_fnptr            dcc=8   obj=10  差=2
O2 seg_qualifier.c           typeof_test          dcc=11  obj=13  差=2
O0 fenv.c                    test_fenv            dcc=125 obj=126 差=1
O2 fenv.c                    test_fenv            dcc=133 obj=134 差=1
```

逐条三分定性（`dc` = `-S` 文本自数；`as` = 那份文本被 GAS 回炸后再 objdump 数；
`obj` = `dcc -c` 直发对象 objdump 数）：

- **14 条 `dc == as ≠ obj`** —— 那份文本自身完全自洽（回炸后一条不差），差在
  `dcc -S` 与 `dcc -c` **两条产出路径发的不是同一串代码**（典型：
  `static_local_arrays.c` 四个函数、`static_agg_by_value.c main` 593/625、
  `builtin_misc.c test_jmp` 44/16）；
- **2 条** `sched_rq_lock_patterns.c raw_spin_lock`：回炸在 `lock mov $0, (%r12)` 上
  终止（`as` 那一侧根本没数出来），而 objdump 在 `-c` 对象里读作
  `lock movl $0x0,(%r12)` —— 即**产物本身**有一条 `lock` 贴在非 lockable 指令上。

**两条都不是解码器错位。** 按任务书没有为了让门绿而放宽：探测器以「只报告不断言」落地
（默认 `SKEW_ASSERT=0`，脚本注释写明了未进断言的实测数字与理由），
分母为 0（`skew_cmp == 0`）判红，理由注释照搬已有的指令行分母那一条
（「一定不是零错位，而是探测器自己坏了：objdump 不在 PATH、`-c` 全挂、awk 脚本名写错
都会造成这个形状」）。等 `-S`/`-c` 两条路径对齐、skew 回到 0，再把默认值改 1。

### 5.4 全语料 GAS 回炸（修前 → 修后）

同一钉法（`as -o x.o <dcc -S 输出>`），317 个顶层夹具 × `-O0/-O2`：

| 首条错误 | 修前（19:01） | 修后（23:31；00:20 重跑一致） |
| --- | --- | --- |
| `expecting operand before ','; got nothing`（SIB 出括号） | 23 | 0 |
| `operand size mismatch for 'call'`（少星 / 32 位名） | 15 | 0 |
| `invalid character '?' in mnemonic`（`f???`） | 14 | 0 |
| `no such instruction: 'shift …'`（假助记词） | 5 | 0 |
| `invalid char '<' … '<dcc-unresolved-target-0x…>'` | 8 | 8 |
| `invalid character '\' before operand 1`（`\u0041`） | 2 | 2 |
| `expecting lockable instruction after 'lock'` | 0（被前面的错误挡住） | 2 |
| `symbol '.L0' is already defined` / CFI 未闭合 | 1 + 1 | 1 + 1 |
| **合计失败 / 通过** | **69 / 557** | **14 / 612** |

修后剩下的 14 条里：8 条是本文件的 `Dec::unresolved_target` 占位符（§6.1，**有意**
不静默），其余 6 条在别人的产出路径：2 条符号名里的 `\u0041`、2 条产物的 `lock mov`、
1 条 `.L0` 重定义、1 条 CFI 未闭合。

---

## 6. 需要主控决策的点

1. **`<dcc-unresolved-target-0xNN>`（8 条，在本文件里）**：这是
   [`Dec::unresolved_target`](../src/codegen/disasm.rs)（`src/codegen/disasm.rs:619`）的
   **故意**拼法，不是漏修的缺陷：旧写法 `format!(".L{}", addr)` 造出一个「语法合法但从
   未定义」的标号，GAS 安静地发一条 `R_X86_64_PLT32/.L126` 重定位 ⇒ 产出的 `.o` 能链、
   能装、跑必挂；`<>` 不是 GAS 表达式合法 token，汇编器直接报错终止，保证「不静默」。
   代价就是这 8 个夹具的 `-S` 文本**永远回炸不绿**。另：`flag_clobber_frame.c` /
   `struct_abi_extended.c` 里的 `0x4cd`/`0x4f5` 恰好就是**下一条指令的地址**（`je` 到
   下一个标签），说明是 `-S` 侧 `labels` 表少登记了这类站点。三个选择：
   (a) 让 `-S` 路径把这些站点补齐到 `labels`/`disasm_calls`（属他人文件）；
   (b) 在回炸门里把 `<dcc-unresolved-target-…>` 行白名单掉；
   (c) 保持现状（读数诚实、回炸红）。实测数字兜底（`call 0x4cd`）会被 GAS 接受但组出
   `e8 00 00 00 00`（rel32=0）——「合法但字节错」比诚实占位符更坏，所以没改成数字。
2. **无尺寸字母的「内存目的 + 立即数源」94 行**：实测 `as` 只给
   `Warning: no instruction mnemonic suffix given and no register operands; using default
   for 'mov'`，不报错 ⇒ 回炸能过，但组出的宽度是 GAS 的默认值，与 dcc 发的字节宽度
   可能不同（`-S` 读数看着对、回炸产物变义）。本文件「不带尺寸字母」是既有约定，
   要改就是全语料 246839 条纯指令行的文本格式变更（寄存器入内存的 17145 行无此问题，GAS 能反推）。
3. **`lock mov $0, (%r12)`（2 条）**：objdump 在 `-c` 对象里同样读作 `lock movl`，
   即**产物级**缺陷，属 `src/codegen/x86_64/**`（本任务禁止触碰），请另案。
4. **`.L0` 重定义 / CFI 未闭合（各 1 条）**：`ms_ext.c`、`cfi_directives.c` 的 `-S`
   文本标签/CFI 产出问题，同样在他人文件。
5. **回炸门要不要扩到全语料**：现在只有 `vec_small_return.c` 一个夹具（§1.4），
   本轮 4 个缺陷它一个都抓不到；扩到全语料需要 626 个组合 × `as`（实测约 4 分钟）。
6. **`C6 /7`（`xabort`）/ `C7 /7`（`xbegin`）/ `0x8F`（`pop r/m64`）不在这张表里**：
   objdump 都有名字（`xabort $0x1`、`xbegin 2c <xc7_f8+0x7>`、`pop (%rax)`）。从代码读：
   `C6`/`C7` 两臂不看 `/digit`，`c6 f8 01` 会被打成 `mov $1, %al`（**长度对、名字错**，
   且这份文本 GAS 能接受但组出的是 `mov` 的字节）；`0x8F` 在一字节主图里没有臂，
   `Dec::unknown_len` 把它与 `C4/C5/62` 一起当 VEX/EVEX/XOP 前缀消费 payload。
   可达性不是问题：后端确实发 `C7 F8`（`src/codegen/x86_64/asm/emit_lines.rs:1038` 的
   `xbegin`、`:1064` 的 `xabort`），`tests/fixtures/asm/asm_avx.c`、`asm_operand_patterns.c`
   等夹具也在用；但 `check_rex_hygiene.sh` 与本门的语料都是 `tests/fixtures/{c,opt}`
   （脚本里自己写了不含 `tests/fixtures/asm`），所以这条路径上没有任何读数在看着它。
   要不要补？没单方面动的原因是：`xbegin` 的目标得选一种拼法（objdump 打符号+偏移，
   本文件的转移目标走的是 `Dec::jump_target`，接到 `C7 /7` 上会连带影响 §6.1 那个
   占位符策略）。

---

## 附录 A：真值钉法（可复现）

```sh
# x87 内存形 64 格（mod≠3）、寄存器形 512 格（mod=3）、间接转移、ALU、FE/FF 同一钉法：
# 每格一个小函数 + 尾接 0xc3，这样 objdump 的读数不会因上一条长度错而连带歪。
{ echo '.text'; for op in d8 d9 da db dc dd de df; do for r in 0 1 2 3 4 5 6 7; do
    printf 'm_%s_%s:\n  .byte 0x%s\n  .byte 0x%02x\n  .byte 0xc3\n' "$op" "$r" "$op" $((0x$r<<3));
  done; done; } > /tmp/memx87.s
as -o /tmp/memx87.o /tmp/memx87.s && objdump -d --no-show-raw-insn /tmp/memx87.o
# 512 格只多一层 rm 循环（mod=3 ⇒ modrm = 0xC0|reg<<3|rm）；本文的 155/(bad) 格与
# 按 opcode 分布就是从这份输出里数出来的（.work_tmp/seg_probe/reg512.o）。
# §3.1 那张表 = 把上面的输出「标签行与首条指令行」并成一行（`ret` 行丢掉）：
objdump -d --no-show-raw-insn /tmp/memx87.o | awk '
  /^[0-9a-f]{16} </{lbl=$0; got=0; next}
  /^ +[0-9a-f]+:/  { if (!got && $0 !~/\tret$/) { print lbl "    " $0; got=1 } }'
# 对账（本文表与 objdump 的指令列逐列比较，只收拢空白，实测 64/64 一致）：
#   paste -d'|' <(本文表 | norm) <(上面输出 | norm) | awk -F'|' '$1!=$2{n++} END{print n+0}'
#   norm = awk '{i=index($0,">:"); s=substr($0,i+2); gsub(/[ \t]+/," ",s); sub(/^ /,"",s); print s}'
# 抓 GAS 消息必须 LC_ALL=C，否则本机 locale 会打成「错误：」而抓不到原因
LC_ALL=C as -o /tmp/t.o /tmp/t.s 2>&1 | grep -m1 -i error
```

三个坑（都踩过）：`.byte` 必须写 `0x..`（`.byte 0{b:02x}` 会被当浮点数报
`floating constant`）；GAS 标签不能以数字开头（`8f_00` 不行，写 `pop_00`）；
`objdump` 要 `--no-show-raw-insn`，否则原始字节列会让「一行一条指令」的计数正则数歪。

## 附录 B：本轮没碰的东西

`src/ssa.rs`、`src/opt.rs`、`src/parser.rs`、`src/codegen/x86_64/**`、`src/link/**`、
`src/driver.rs`、`tests/**` 一律未改（工作树里这些文件的在途改动是别人的）。本轮
`cargo build --release` 与两条测试过滤**没有**遇到来自他人文件的编译错误，
没有用 `--skip` 绕过任何判据，没有 `cargo clean`、没有删文件、没有任何 git 写操作。

收尾时另外动过的两处（都不改语义）：

- `src/codegen/disasm.rs` 跑了一次 `rustfmt --edition 2021`（只作用于本文件）。HEAD 版本
  是 fmt-clean 的，本轮与上轮的改动把它顶出了 20 个 hunk，所以归位；**字符串字面量与
  表内容一行未动**（本文件的判据把 600+ 条 objdump 读数原文钉在断言里，重编后
  `disasm` 10 passed / `rex_matrix` 15 passed 全过，这就是“没动到字面量”的证明）；
  归位后全文件 3862 行（原 3850），文中引用的行号已同步。
- `scripts/check_disasm_coverage.sh` 只改探测器那一段（`bash -n` 通过），
  `Makefile` 只改 `disasm-cov` 目标块（`git diff -- Makefile` 只有一个 hunk，
  已核对：`CEILING=13` → `CEILING=0` 与块内注释）。
