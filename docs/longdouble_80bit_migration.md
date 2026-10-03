# a6：long double 的 80-bit 值模型三侧迁移

> **路径勘误（2026-10，SSA 目录化）**：本文清点消费点时列的 `ssa.rs`（以及它旁边的 `ir.rs`
> 指 `src/ir.rs`，不是新文件 `src/ssa/ir.rs`）是当时的事实，**不改写**；`src/ssa.rs` 现已拆为
> `src/ssa/` 多个职责文件。对照表见 [`docs/ssa-module-layout-plan.md` §10](./ssa-module-layout-plan.md)。

## 当前状态（本文件开头以下的历史分析已部分过时，以此处为准）

| 面 | 状态 |
|---|---|
| 局部对象表示 | **已是真 80-bit**（`fldt`/`fstpt`），“低 8 字节 = f64、高 8 = 0”不变量已取消 |
| rvalue 值通道 | **已是 st(0)**（`float_in_st0` 对 `LongDouble` 恒真） |
| 算术 / 比较 | **已是 x87 80-bit**，除法/乘法/累加/取负的 `%Lf` 数字与 gcc 逐字符一致 |
| 跳边界（变参/具名形参/返回值） | 已修；D9 的降级垫片已简化为单条 `fldt` |
| 表达式常量→`.rodata` | 已发 80-bit 编码（`fldt` 读回） |
| 全局量 `.data` 初始化器 | **发射侧已修**（字节与 gcc 逐一致）；**读侧仍坏**，见下 |
| 十进制字面量的**值** | 仍经 `Expr::Float{v: f64}` 解析（需 AST `Constant` 枚举化） |

### 发射侧（已修）：`float_data_bytes` 单一真源

`Codegen::float_data_bytes(v: f64, size: usize)`（`emit/expr/utils.rs`）是浮点
初始化器→数据字节的**唯一真源**，`size == 16` 走 `float80::from_f64` 发真 80-bit
编码 + 6 字节 padding。已接入的站点：

- `emit/expr/utils.rs` `eval_const_expr` 的 `ConstValue::Float` / `Int` 两支（数组元素、InitList）
- `emit/expr/utils.rs` `Codegen::global_init_bytes`（被 `elf/init_list.rs` 调用）
- `mem/global.rs::global_init_bytes`（自由函数，被 `codegen.rs` 三处 + `static_locals.rs` 调用）

**这两个 `global_init_bytes` 是两份都活着的副本**，只改一份的后果是“编译通过 +
cargo test 全绿 + 现象一点不变”——已实际踩到，靠“现象是否变化”而非“测试是否绿”才能发现。

验证：`long double h = 1.0L;` 的 `.data` 现在是
`0000000000000080ff3f000000000000`，与 gcc 逐字节一致（以前是 `...f0 3f` + 8 零的 f64 位型）。
`long double arr[3] = {2.5L, 3.0L, 7.0L}` 的 `%.27Lf` 输出已与 gcc 完全一致（以前全打 0.0）。

### 仍未修：`long double` **全局量**的读侧仍用 `movsd`

字节已经对了，但读回来仍是 `-nan`，因为全局 lvalue 的 load 还在按旧不变量取低 8
字节当 f64。两侧反汇编：

```
gcc : fldt   0x2edc(%rip)   # 404020 <h>      ← 读 10 字节 80-bit
      fstpt  (%rsp)
dcc : movsd  0x2159(%rip),%xmm0  # 4032c0 <h>  ← 只读低 8 字节，且当 f64 位型
      fstpt  (%rsp)                            ← 把垃圾拓宽，打印为 -nan
```

注意 `mem/access.rs:492-496` 与 `594-596` 处的同类问题**已经修过**（注释里写着
“旧写法对 LongDouble 发 movsd，只拿到低 8 字节”），那些是**局部**变量路径。
全局路径的读站点还在第三处，本次没找到。下一步：从 `emit/expr/lvalue.rs:603/694`
与 `lvalue.rs:465/482/527/624` 这几个 `Float|Double|LongDouble` 分支里找“按
is_float 统一发 movsd”的那一个，给它加 `float_in_st0(ty)` 分支（发 `fld t` 到
st(0)，而不是 movsd 到 xmm0）。局部已经对了，可以直接对照局部的生成序列。

临时验证命令（修好后应逐字节一致）：

```bash
printf '%s\n' 'long double h = 1.0L;' 'extern int printf(const char*,...);' \
  'int main(void){ printf("%.27Lf\n", h); return 0; }' > /tmp/ldg.c
gcc -std=gnu17 -O0 -no-pie -o /tmp/ldg_g /tmp/ldg.c && /tmp/ldg_g
target/debug/dcc -std=gnu23 -no-pie -o /tmp/ldg_d /tmp/ldg.c && /tmp/ldg_d
```

### 未修：`long double` 全局量带静态初始化器读回 `-nan`

`.data` 发射仍写 **f64 位型 + 8 零**，而读侧现在用 `fldt`，于是把 f64 位
型当 80-bit 解释：

```
$ gcc -std=gnu17 -O0 -no-pie  : g=0.100000000000000000001355253  h=1.000000000000000000000000000
$ dcc -std=gnu23  -no-pie     : g=-nan                          h=-nan
$ objcopy -O binary --only-section=.data:
  gcc 初始化 long double g = 0.1L → cdccccccccccccccfb3f000000000000   （真 80-bit）
  dcc 同一源                      → 9a9999999999b93f0000000000000000   （f64 位型）
```

注意连 `1.0L` 都是 `-nan`（f64 的 `00*6 F0 3F` 当 80-bit 读时，指数域落在高两
个零字节上 → 指数 0 且整数位为 0 → 非法/伪次正规）。这是**对象表示迁移留下的
中间态**：以前读侧用 `movsd` 取低 8 字节恰好自洽，现在读侧变 `fldt` 就不自洽了。

修法：`.data` 发射处把 f64 值拓宽成 80-bit 编码再写 10 字节。可直接复用
`src/float80.rs` 中已经逐字节验真的编码/`from_decimal`（13 例全对，含次正规与
最大有限值）。

**为什么 `cargo test` 没抱住它**：2579 项全绿，因为**没有任何夹具覆盖“带静态
初始化器的 `long double` 全局量”**。这跟刚修掉的 `ok` 被覆盖事件是同一类
盲区：绿灯只证明已测到的部分对，不证明覆盖到了该轴。修此 bug 时应同时把
上面的 `g=`/`h=` 探针固化成夹具。

## 以下为迁移前的历史分析（保留作为记录，结论可参考、现状以上表为准）

SysV AMD64 上 `long double` 是 x87 扩展精度：64 位显式整数位尾数 + 15 位指数，
占 16 字节对象（align 16，`b[10..16]` 是实现定义 padding，GCC 不保证为零）。

dcc 当时把它当成 f64 存算。实测（`long double g = 0.1L;` 的 `.data`）：

```
gcc:  cd cc cc cc cc cc cc cc fb 3f 00 00 00 00 00 00   尾数 …CD，指数 0x3FFB
dcc:  9a 99 99 99 99 99 b9 3f 00 00 00 00 00 00 00 00   f64 位型 0x3FB9… + 高 8 零
```

dcc 产出的不是任何 x87 消费者能读的编码。跨编译器读取 dcc 导出的 `long double`
全局量必然得到垃圾值；`0.1L` 这类字面量也永久丢尾数低 11 位。

## 根：AST 字面量通道是 f64

```rust
// src/ast.rs:364
Float { v: f64, span: Span, ty: Type },
```

任何 80-bit 值都必须穿过这个 `f64` 字段，所以 `src/float80::from_decimal`
（已实现、9 个单元测试全绿、**零调用者**）在通道加宽之前无法被使用。
这是整个迁移的前置瓶颈，也是"D3/D5 那一类判据分散"之外的**结构性**障碍。

## 已有资产（不要重做）

| 资产 | 位置 | 作用 |
|---|---|---|
| `float80` 模块 | `src/float80.rs`（`lib.rs` 已声明） | `from_decimal` 用精确整数运算做 RNE，不借中间浮点近似；`to_f64`/`from_f64`/`negate`/`abs`/`is_zero`/`exp_field`/`mantissa`；`Bits=[u8;10]` 小端镜像。**已对 gcc 逐字节验真**：`decimal_literals_match_gcc_byte_exact` 用 13 个字面量（含最小正次正规 `3.645105638354799e-4951L` → `01 00…`、最大有限值 `1.189731495357231765021e+4932L` → `ff…fe7f`）比对 `objcopy --only-section=.data` 抽出的前 10 字节，全部一致。发射侧可直接复用该位型输出。 |
| x87 tbyte 内存存取编码 | `emit/expr/float.rs:341/351`（`0xDB 0x3C 0x24` = `fstp tbyte [rsp]`，`0xDB 0x2C 0x24` = `fld tbyte [rsp]`） | 80-bit 内存 load/store 的指令编码已在树内被验证，无需从零写 x87 |
| 16 字节布局 | `ast.rs:1169/1241`（size=align=16） | 对象尺寸与对齐**已经正确**，缺的只是内容与存取宽度 |
| ABI 分类 | `abi/tyclass.rs`（`is_real_float` 对 LongDouble 返回 `X87`） | 分类轴已正确；`va_start` prologue 已用 `fstps/fstp tbyte` 保存 xmm0-7 到 FP 区 |

## 三侧落点

### 侧 1 — 前端：字面量解析
- `parser.rs`：`L`/`LF` 后缀与 `_Decimal128` 映射（829、1121、2150、6321、6911、7157-7158）
- `ast.rs:364` `Expr::Float{v: f64}`、`ComplexLit{real: f64, imag: f64}`
- `const_eval.rs:386`（浮点常量折叠）

### 侧 2 — 数据发射
- `emit/expr/utils.rs:447` `eval_const_expr`，其中**明确写着**
  `// dcc LD: 16B 槽 = 低 8B double 位模式 + 高 8B 零`
- `elf/init_list.rs`：38、121、218、384、430、493、525（数组/结构体/静态初始化）
- `elf/static_locals.rs`：34、94

### 侧 3 — codegen
- `emit/expr/float.rs:47`：`// LongDouble is internally stored/computed as double in xmm0`
- 中心 load：`float.rs:662` `load_stack_xmm(0, src_off, 8, false)`（`Double | LongDouble` 共用）
- 中心 store：`float.rs:740` `store_xmm64_stack(0, dst_off)`
- 转换：`float.rs:121/174/297/518/577/627/675/832`
- 影子风险：`emit/expr/dispatch.rs`(47 处) 与 `emit/expr/init.rs`(20 处) 是**已知逐字双份**，
  `complex.rs`(23 处) 同类。任一侧改动必须同步，或先收敛。

## 为什么必须原子、不能只改一侧

只改侧 1+2 会让字面量按 80-bit 舍入并入槽，而侧 3 仍按"低 8 字节是 f64"来
load/store/比较。结果是**同一个对象两种解释**，比现在"统一按 f64"更糟：
现在至少自洽（只是精度与跨编译器互操作不对），部分迁移会直接产生错值。

## D9 已修复（变参 `long double` 跨界表示）

上一轮定位、本轮落地。两侧同批改动：

- **caller**（`abi/call.rs` 的 `LongDouble` 分支）：新增 `ld_in_variadic` 判据
  （`variadic_funcs` 且 `arg_idx >= func_param_types[name].len()`），变参位置改为
  `movsd [rsp+off+8]` → `fld qword [rsp+off+8]` → `fstp tbyte [rsp+off]`，
  用参数自己 16 字节槽的高半当 scratch（不越界、不移动 rsp）。
- **callee**（`va_arg_store.rs`）：新增 `emit_ld80_narrow_to_xmm0(base_r9)`
  统一三处读侧（named 用 r9，global / ptr 用 rcx），避开第 4 份副本。
  读侧 scratch 必须用 `sub rsp,16` 而非回写目标槽：`overflow_arg_area` 指向
  **调用方**栈帧，写回去会破坏其他参数与 `va_copy` 后的重读。

验证：`tests/fixtures/c/va_arg_mix/ld80_vararg_*.c` **四个方向全绿**
（全 gcc / 全 dcc / dcc callee+gcc caller / gcc callee+dcc caller），
2577 个测试零失败。具名形参的 `long double` 仍按旧约定压栈（应在 st(0)），
那是下述独立缺陷，本次未动。

## 另案：具名形参 —— 已用 gcc 实测推翻本文件早先的错误结论

早先本节写过“具名形参的 `long double` 按 SysV 应在 **st(0)** 传递”。**这是错的**，
那是 i386 cdecl 的规则。gcc `-O0` 实测（objdump）：

```asm
; caller                              ; callee
lea -0x10(%rsp),%rsp                 fldt   0x10(%rbp)    ← 从栈上读
fstpt  (%rsp)                        fldt   0x20(%rbp)
lea -0x10(%rsp),%rsp                 ; 只有**返回值**用 st(0)
fstpt  (%rsp)
```

SysV AMD64：x87 **参数走内存**（每参 16 字节栈槽，内容是真 80-bit），
仅 **返回值** 用 st(0)。所以 dcc 把形参放栈上、每参 16 字节这两点**本来就是对的**。

已拆为 D10 三处独立缺陷，复现器 `tests/fixtures/c/va_arg_xfail/ld80_namedparam_*.c`，
四方向实测：全 gcc=0 / 全 dcc=0 / **dcc callee+gcc caller=139(SIGSEGV)** /
**gcc callee+dcc caller=1**。详规与修法则见该文件头注释。要点：

1. caller：直接**放开** D9 新加的 `ld_in_variadic` 判据（当时因错误假设而多加了限制），
   具名位置共用同一段 `movsd scratch → fld qword → fstp tbyte`。
2. callee 入口：在 prologue 处**降级一次**（`fldt [入参槽]` → `fstpl [局部槽]` +
   局部槽高 8 字节清零），以保住“局部低 8 字节 = f64”这个不变量。这比去改
   任何 long double 左值的 load（那会牵动 58 个调用点）便宜得多 —— 与 D9 同样
   的“只在边界转换”思路。代价：`stack_param_stores` 目为 `Vec<(i64,i64)>`，
   需多携带一个“是否 long double”标得改约 8 个 push 站点。
3. 返回值：需 st(0)，解释 gcc callee + dcc caller 方向的失败。是三者中最独立的一块。

## 仍待做的三件事

1. **字面量精度**（真正需要 AST 加宽的那件）：`0.1L` 等仍先 `parse::<f64>()`
   再展宽，丢尾数低 11 位。需先完成下面的 `Constant` 枚举化。
2. ~~**具名形参应在 st(0)**~~ → **已由 gcc 实测推翻**，实际为 D10（栈上 16 字节但
   内容需 80-bit + 入口降级 + 返回值用 st(0)），见下节。
3. **80-bit 算术与比较**：目前全走 xmm0-double，只在跨界处转换。

## 已有资产补充：`emit_ld80_narrow_to_xmm0` 可复用

修复 D9 时沉下来的这个 helper 就是“80-bit 对象 → f64 值通道”的唯一真源，
上面三件事的读侧都应调它，不要再写第二份转换序列。

## 已验证的新发现：存在一个不碰 AST 的更小前置增量（D9）

本轮混合链接实测（复现器：`tests/fixtures/c/va_arg_xfail/ld80_vararg_*.c`，
全 gcc 退出 0 / dcc callee + gcc caller 退出 1）：

SysV 下 `long double` 属 **X87 类**，变参实参**永远**放在 16 字节对齐的
`overflow_arg_area`（内容是 `fstpt` 写出的真 80-bit 编码），**不进寄存器保存区**。
而 `gen_va_arg_long_double` 发的是 `movsd (%r9), %xmm0` —— 只取低 8 字节当 f64
位型，实际读到的是 64 位**尾数**，`(long long)` 转换得 LLONG_MIN。

为什么它比整场迁移便宜：它只修**跨界表示**（caller 存 + 3 个 reader 变体读），
内部计算仍按 f64/xmm0 语义，所以**不需要改 AST 字面量通道**，那 62 处一处不碰。

但必须两侧同批：dcc caller 与 dcc callee 目前是**自洽的错**（两边都按
"8 字节 double + 8 字节零填充"约定，见 `abi/call.rs:541` 注释），单改 callee 读侧
会立即破坏 dcc↔dcc：对 f64+零填充的槽做 `fld tbyte` 会得到指数字段为 0 的
次正规值。这也意味着：**单文件差分夹具永远查不出这类问题**，必须混合链接。

转换序列（x87 编码已在树内被 `float.rs:341/351` 验证过）：

```asm
fld tbyte [r9]        ; 41 DB 29          读：80-bit -> st(0)
fstp qword [rsp]      ; DD 1C 24          降到 f64（硬件舍入）
movsd xmm0, [rsp]     ; F2 0F 10 04 24
; 写侧相反：fld qword [src] ; fstp tbyte [槽]
```

需一个 8/16 字节 scratch。本文件族已有 `sub rsp,16 / [rsp] / add rsp,16` 的先例
（`gen_float_binop` 的 -mno-sse 路径），rsp 在表达式内平衡使用，对上层不可见。

另案：具名形参的 `long double` 按 SysV 应在 **st(0)** 传递，而 dcc 现在按自定
约定压栈（`call.rs:541`）—— 与上面的变参路径是**两个独立缺陷**，不得合并修改。

## 唯一自洽的最小落地单元

### 设计约束：AST 加宽必须选「编译期强制」而非「可选旁路字段」

`Expr::Float` 在树内被构造/match **62 次**，散布于 `parser.rs`、`const_eval.rs`、
`ssa.rs`、`ir.rs`、`opt.rs`、`sanitizer.rs`、`cpp/lower.rs`、`linker_script.rs`(11)、
以及 codegen 各文件。

- 若加**可选旁路字段**（`Expr::Float { v: f64, bits80: Option<Bits>, ty, span }`）：
  只有构造点会编译报错，而 `opt`/`ssa`/`ir` 里那些**从 `v` 重建 `Expr::Float`** 的
  改写 pass 会静默把 `bits80` 填成 `None`。80-bit 位型在某个优化后被无声丢弃，
  表面看迁移已完成、测试也可能过（因为回退到 f64 就是今天的错值），
  属于与「影子副本」同一类的隐形丢失缺陷。
- 若改成 `Constant` 枚举（`Float{f64} | LongDouble{Bits} | …`）：每个消费点都必须
  显式处理 80-bit 分支才能过编译，丢失会变成编译错误而不是错值。

结论：走 `Constant` 枚举（或等价的「类型即变体」建模），不要加旁路字段。
代价是 62 处都要过一遍，但这正是它便宜的地方 —— 漏一处会编译失败。

### 落地内容

一次提交内同时完成：

1. AST 加宽字面量通道，携带 80-bit 位型（`Option<Bits>` 附加字段，或 `Constant` 枚举）
2. 侧 2 三处发射点改为写真实 10 字节 + 6 字节 padding
3. 侧 3 的中心 ld/st 改为整 16 字节搬槽（值在内存里是 80-bit，不能再按 f64 读低 8 字节）
4. 需要参与**计算**的场合经 x87 `fld tbyte`/`fstp tbyte`（编码已验证）进 st(0)，
   或显式 `float80::to_f64` 降级；比较/算术的 80-bit 全精度可作为后续增量

侧 3 的第 3、4 点是工作量大头，且必须先盘点 xmm0 与 st(0) 两个值通道的交界，
以及 `-mno-sse` 路径（float.rs:570 已有 x87 分支）。

## 算术/比较侧：不等字面量迁移，今天就有序位错

旧判断“字面量仍走 f64 通道，所以改算术得不到精度收益”**只对了一半**。
实测（`%Lf` 按真 80-bit 打印，作 gcc oracle）：

```
gcc:  1.0L / 3.0L = 0.333333333333333333342368351   ← x87 80-bit，64 位尾数
dcc:  1.0L / 3.0L = 0.333333333333333314829616256   ← f64 的 1/3 再拓宽
```

`1.0L` 与 `3.0L` 在两种精度下**都精确**，但**商本身需要超过 53 位**才能表示。
同类：除法、开方、乘法产生的新低位、以及误差累积（循环叠加会把差异放大）。
所以**纯算术迁移就有独立、可测的收益**，不以字面量迁移为前置。

**真正的前置是对象表示。** 若局部槽仍是“低 8 字节 = f64”，则：

```
fld qword [slot]   → x87 上有真 80-bit 操作数（f64 → 80-bit 是精确的）
fdivp              → 得到真 80-bit 商     ← 到这一步已经对了
fstp qword [slot]  → 又四舍五入回 f64   ← 在这一步全丢
```

即：**只要 store 仍是 `fstp qword`，算术改成 x87 也看不到收益**。因此必须同时：

| 项 | 现状 | 目标 |
|---|---|---|
| ld 对象 load | `movsd` 或 `fld qword` | `fld tbyte` |
| ld 对象 store | `movsd`（高 8 字节手工清 0） | `fstp tbyte` |
| 二元运算 | `addsd/mulsd/subsd/divsd` | x87 `fadd/fmul/fsubr/fdivp` |
| 比较 | `comisd` 系列 | `fcomip` / `fcom` + `fnstsw` |
| 高 8 字节不变量 | “必须为 0”，~58 处调用点依赖 | **删除该不变量** |

附带收益：跨界也不再有损。目前 gcc 函数返回的真 80-bit 值被 dcc 存进局部时
会经 f64 舍入（D10-2 入口降级与返回值物化都是 `fstp qword`）；对象改真 80-bit 后消失。

执行约束：这是**改变全局不变量**的操作，必须一次提交内原子完成，否则中间态
必然比现状更错（写入 `fstpt` 而读出 `movsd` 之类）。先建立上面的 `q=` 失败基线，
改完必须同时看退出码**与 `%Lf` 输出的每一位数字**——只看退出码仍会假绿。

## 验证要求（每条增量都要满足）

- `cargo test` 全绿；**release 与 debug 都要重链**再跑差分（已踩过 stale release 报假红）
- `.data` 逐字节比对 gcc：至少 `0.1L`（尾数 `…CD`、指数 `3ffb`）、`1.0L`、负数、
  次正规、`__builtin_huge_vall`
- 交叉侧验证：dcc 生成的 `long double` 全局量由 gcc 读取，值必须一致
- 判读机器码只准 objdump（`dcc -S` 会把 `movups` 标成 `movsd`、把带 REX.B 的编码
  标 UNKNOWN 导致整段脱轨）
- 退出码判据不得被 8 位截断伪装成 0
