# P12 — 物理寄存器家分配与帧偏移守门人 / 值通道统一

> 本文档是 `src/ssa/regalloc.rs` 与 `src/codegen/x86_64/` 两侧设计的入口。
> 代码注释里散落的「§7.x」指向 `docs/ssa-pass-upgrade-plan.md`；本文件把
> 物理寄存器家（physreg home）分配的**现状**与帧偏移守门人 / 值通道统一
> （`x86_64-frame-offset-gatekeeper-and-channel-unification.md`）的落地结果
> 合在一处，避免读代码时反复跳文件。

## 1. 物理寄存器家（physreg home）

SSA→AST 重建把同基名的多个版本塌缩回同一个可变量，读写的**先后**就是语义，
因此分配粒度只能是变量（基名）。这也决定判定偏保守：活性区间取「该基名在
IR 里最早出现的位置 ~ 最晚出现的位置」，跨分支、跨回边一律按最坏情况算重叠。
少绑一个变量只是少一点收益，多绑一个就是静默错值。

### 1.1 准入条件（每一条都对应一个实测过的失败形态）

1. 只处理局部 `Decl::Var`，且类型是 1/2/4/8 字节的标量整数或指针。
   浮点要换 xmm 通道（后端没有为它准备的宽度口径），聚合/数组/向量本质是
   内存对象，`long double` 是 10 字节对象，`_Atomic` 与位域另有语义。
2. 地址被取走过 ⇒ 不绑。判据复用 `ast_gvn::addr_escaped_names`，绝不自己
   再爬一遍 AST —— 两套逸出判据里弱的那一套会决定产物。
3. 出现在任何内联汇编的操作数里 ⇒ 不绑。asm 输出写回虽然已经认识绑定表，
   但 `"+r"`、`"=m"` 与「栈槽从此没人写」的组合还缺一整批实测，不把未验证
   的形态交给自动分配。
4. 函数体里有嵌套函数 ⇒ 整个函数不绑：嵌套函数按帧偏移访问外层变量，而
   绑定变量的那个偏移没人写。
5. `static` / `_Thread_local` / 纯 `extern` / `volatile` / `cleanup(...)` /
   `__asm__("label")` 过的变量不动；用户已经手写 `register asm` 的名字更
   不动（它占的那个号也从池子里扣掉）。
6. `-g` 不再拦本 pass：Phase 6.2 接线后 `src/debug.rs` 已对每个物理寄存器
   家的局部发 `DW_OP_regN`（见 `register_homes_allowed` 与钉住用例
   `debug_info_no_longer_gates_the_register_home`）。
7. 名字在函数里被**重复声明且类型不同**（遮蔽）⇒ 不绑：后端按名字共用一个家。
8. 基名的**第一次出现必须是 entry 块里的定义**：
   - 只看「先定义」会放过 `int x; if (c) x = 1; return x;` 这种「某条路径
     才初始化」的变量，未初始化的那条路径就会读到调用者留在寄存器里的值
     （-O0 读到的是栈垃圾）—— 同一个未定义行为在两个档位给出两个确定值，
     等价性门禁就变成抛硬币；
   - 形参也不行：它的值由 ABI 送进来，在 IR 里的第一次出现是使用而不是定义。

### 1.2 候选池

- 基础池 `POOL`：GP callee-saved / caller-saved 中不被发射侧当固定暂存号
  的号。
- 延伸池 `AUTO_EXT_POOL`：默认 `{r8, r9}`。`rcx` 已否决（被
  `gen_expr_decay(r, 1)` 当二元运算右操作数的固定暂存号）。
- `DCC_EXT_POOL` 环境变量可逐号裁剪；设为 `0` 即完全关池用于对比。
- 审计入口：`.work_tmp/p12_base/audit_ext_pool.sh`。

## 2. 帧偏移守门人（frame-offset gatekeeper）

x86_64 帧分配由 `SretRule` 枚举 + `reconcile_frame_demand` 扫描单一真源驱动：

- **位域 RMW（P0）**：H1/H2 几何下不再手写帧偏移；`bf_rmw_slot(depth)` 带
  边界断言。
- **trampoline band 动态预留（P2）**：`vec_need = max_vec_chain_depth × slot
  字节`，替代原固定 512B；浅表达式函数省下大部分，深链函数按真实深度给足。
- **Decl 初始化 save/restore 与 sret watermark 配对（P1 follow-up）**：
  `bf0bc38d`。
- **诚实预留（P3）**：`stack_reserve = 0`（动态暂存全部由 `emit_sub_rsp`
  自给）。

所有偏移访问器从带表派生：`vec_temp(slot,size)`、`bf_rmw_slot(depth)`、
`tramp_slot_off(i)`、`spill_off(slot)`、`late_*`、`sret_region_base /
sret_band_bottom()`。

## 3. 值通道统一（channel unification）

双通道（GP / x87 `st(0)`）经 `emit_operand_to_st0` 单一交界入口：

- `float_in_st0(&Type)` 是「这个类型的 rvalue 在 st(0) 吗？」的**单一谓词**，
  识别标量浮点（`Float`/`Double`/`LongDouble`）与 `Type::Imaginary(inner)`
  并委托 `inner.base`。
- `emit_int_to_st0` 是整数→st(0) 的唯一路径，发射
  `sub rsp,32; mov [rsp],rax; fild qword [rsp]; add rsp,32`。

### 3.1 `_Imaginary long double`（P4）

- `_Imaginary long double` 走 x87 st(0) 通道（与 `long double` 同通道）。
- SSA→AST 重建（`src/ssa/expr_rebuild.rs`）与 `parser.rs` 二元类型推导一致：
  - `imag ± imag → imag`
  - `imag * imag → real`（取负）；`imag / imag → real`
  - `real ± imag → complex`
  - `real * imag → imag`；`real / imag → imag`
- 旧实现把 `_Imaginary` 与 `_Complex` 混为一谈一律返回 `Complex(inner)`，
  导致 O2 下 `_Imaginary long double + _Imaginary long double` 被标注为
  `Complex(LongDouble)`，消费端 `emit_operand_to_st0` 认为它不在 st(0) 通道，
  转而 `emit_int_to_st0` → `fild qword [rsp]` 把 `faddp` 算好的 st(0) 覆盖
  成 `rax` 里的垃圾（REX 卫生门禁抓的就是它）。

## 4. 验证门禁

| 判据 | 命令 / 方法 |
|---|---|
| REX 卫生 | `bash scripts/check_rex_hygiene.sh`（678 个「夹具×档位」组合零违规） |
| 行为等价 | `bash scripts/check_opt_equivalence.sh`（四档） |
| 产物可复现 | `bash scripts/check_reproducible_output.sh`（三档哈希） |

语料：`tests/fixtures/c` 与 `tests/fixtures/opt`。
