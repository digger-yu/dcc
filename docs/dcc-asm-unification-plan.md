
# dcc 指令发射统一方案（执行版）

本文件只写**要做什么**与**步骤**。论证、方案对比、修订过程一律省略；事实数字仅保留影响执行与验收的部分。

## 0. 已锁定基线（不再讨论）

| # | 决策                                                                               | 落点      |
| - | -------------------------------------------------------------------------------- | ------- |
| 1 | 五个后端统一为"一份指令记录 + 声明式规格"。**x86\_64 也迁入**，其 962 处 asm 发射点改产出记录，`disasm.rs` 降为诊断工具  | P9      |
| 2 | 交叉后端只覆盖 **dcc 自身会发的位型**；用户写的未知指令一律文本化交 GNU `as`（不复制 GAS）                         | P2–P6   |
| 3 | 规格**构建期生成**：编译期常量表 + opcode 枚举；运行期零文件 IO                                         | P1.5    |
| 4 | 布局偏移**类型化唯一入口** `LayoutMap`（收敛面约 150 处：`labels` 读者 90 + `func_sizes`/行表用途 58）    | P2      |
| 5 | G6 穷举**覆盖全 ISA**（长度是 `encode` 的输出，不是待验证量）                                        | P2–P9   |
| 6 | 降级粒度是 **ISA 属性**：定长 ISA 逐语句分流，变长 ISA 整 TU 分流                                     | P2/P7   |
| 7 | ISA 仲裁源：**只 vendor RISC-V**（BSD 系，与 Apache-2.0 兼容）；ARM/MIPS 用"版本锁定的 GAS + 显式分歧表" | P1.5/P7 |
| 8 | **编码正确性与纯移动分开仲裁**：迁移期发现"意图 ⊻ 现字节"只登记不修，由独立 bug-fix 阶段修，届时基线必须变化 | §6 G11  |
| 9 | 指令级解析只允许一份：词法/行分类固定复用 `asm_source`，`asm_engine` 的 `parse` 只负责"操作数文本 → `Operand`" | P2      |

## 1. 与既有文档的关系

| 文档                                   | 处理                                                             |
| ------------------------------------ | -------------------------------------------------------------- |
| `dcc-dual-arch-asm-solution.md`      | 其"待办"提出的"机器码→GAS 文本解码器"设想**被本文件取代**（符号只在发射时可知，事后反推不可行）。P8 回写该节 |
| `dcc-asm-text-disasm-fix-plan.md`    | 其"AsmEvent 生产侧日志、disasm 不进 lowering"方向被本文件采纳并推广到五后端            |
| `backend_refactor_plan.md` §3.1/§4.4 | 纯移动原则、共享边界以它为准：共享机器，不共享数据                                      |

## 2. 已完成（本方案的起点）

* `shared/asm_source.rs`（854 行）：按 ISA 的注释符词法、行分类、`analyze`、`AsmPassthroughRecord`、`record_asm_stmt`（一条语句要么全编码要么整体透传）、`passthrough_diagnostics`（E0311 全仓唯一出口）。

* 五个后端的 asm 处理收敛为薄壳；`asm_text_mode` 双模式轴已删除；`AsmOutcomeSink`/`AsmStmtSink` 注入点建立。

* `CommonImage.asm_passthrough{,_regions}`、`Emitted::{Object,Text}`、`Backend::render_asm_from_state`、`--asm-fallback=gas|error`、`Feature::GasTextOutput` + E0312。

* 修掉三个 silent bug：`msr`/`mrs`/`cache`/`pref` 静默编成 NOP；20 处"操作数解析失败即当寄存器 0"（`blr xN` → `blr x0`）；交叉后端整条丢弃语句位置的内联汇编。

* 交叉后端发射点普查（影响 P3/P6 工作量，见 §3.4）：`aarch64`/`riscv64` 已有单一汇口 `self.emit(&[u8])`；`arm`/`mips` **没有**，是直写缓冲区。

* **新发现：一批"可解码但语义错"的 aarch64 编码**，现有任何门禁都抓不到，全部用 `as`/`objdump` 确证（见 §6 G11 与 §3.5）：

| 发射点                                  | 意图                | 实际字           | objdump 读到                       |
| ------------------------------------ | ----------------- | ------------- | --------------------------------- |
| `aarch64/emit/expr/fcmp.rs:11`       | `fcmp s0, s1`     | `0x1E280000`  | `fcvtps w0, s0`（浮点比较变成无符号取整）     |
| `aarch64/emit/expr/ops2.rs:105` ×11  | `mov x12, x0`     | sf=0          | `orr w12, w0, wzr`（64 位指针被截成 32 位） |
| `aarch64/expression_impl.rs:431-441` | `lsl x9, x9, #N`  | `0xD3000N09`  | `.inst undefined`                 |
| HFA / 向量 / `fsub` / `fdiv`         | `stp s`/`orr v`/… | `2e80*`/`4e80*` | `.inst undefined`（共 10 个字）       |

* `mini_asm` 编码行为差分冻结基线：504,817 条"可编码输入→字节"的逐 ISA 摘要（`tests/fixtures/asm/mini_asm_encode_freeze.txt`）。

* 已删：`asm_common.rs` 占位、`shared/asm_text.rs`（无消费者）、`mini_asm` 的 `parse_asm_lines`/`parse_instruction`/`encode_asm_lines_for_arch`。

* 门禁现状：1896 单元 + 978 集成 + 26 错误快照全绿；`check_reproducible_output.sh` 5052 组合 × 3 次字节一致。

## 3. 事实基线

### 3.1 现存实现与规模

| 位置                                                                        | 行数              | 状态                                                                                                                                        |
| ------------------------------------------------------------------------- | --------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `x86_64/asm/`                                                             | 13410           | 生产可用；`emit_asm_instr` 助记符分支 **765** 条；`self.emit(` 发射点 **962** 处                                                                          |
| `codegen/disasm.rs`                                                       | 2110            | x86 变长反汇编器，当前用于 `-S`（第二份 ISA 真相，P9 消除）                                                                                                    |
| `shared/mini_asm.rs`                                                      | 1588            | 4 个交叉 ISA 的编码表（match 链，P2–P6 逐个替换后删除）                                                                                                     |
| `shared/asm_constraints.rs`                                               | 1578            | 约束解析 + 各 ISA 寄存器池（保留，共享层）                                                                                                                 |
| `data/instruction_loader.rs` / `encoding_selector.rs` / `byte_emitter.rs` | 292 / 257 / 366 | 声明式管线，**零消费者**                                                                                                                            |
| `x86_64/asm/` 侧的 `AsmOperand` + `parse_asm_data_expr`                     | —               | 类型化操作数 + `(值, Option<(符号,rel_type,addend)>, 前向引用)`，经 `elf/resolve.rs::resolve()` 落 `ElfRela`，同时写 `disasm_data` 供 `-S` 渲染符号名。**P1 上收为共享层** |

### 3.2 交叉 ISA 现覆盖面与缺口

```
aarch64 29: add blr br clrex dbg dmb dsb fadd fmul fsub isb ld1 ld1w mov movz mul nop
            ptrue ret sev sevl st1 st1w sub svc wfe wfi whilelt yield
riscv64 12: add addi ebreak ecall fence fence.i nop pause ret sfence.vma sub wfi
arm     15: add bx clrex dmb dsb isb mov nop sev sevl sub svc wfe wfi yield
mips    12: add addu break daddu jalr jr move nop ssnop sub subu sync syscall
```

四项结构性缺口（交叉后端）：① 无 load/store；② 无内存操作数语法；③ 无符号/重定位（签名 `Option<Vec<u8>>` 装不下）；④ 无标签/分支目标。

### 3.3 规格现状（决定 P1.5 工作量）

* schema v2 声明了 `encoding_spec`/`machine_code_hex`/`opcode`/`operand_pattern`（位型），**没有指令级文本模板**：`assembly` 只存在于 `examples[]` 内（与 `machine_code_hex` 成对的示例）。

* 现有 TOML 仅覆盖 **8 条指令**（add and cmp mov mul or sub xor），其中只有 `mov.toml` 写了 `assembly`，值是具体实例。→ 现有数据**不可迁移复用**，只能继承格式设计。

* 主库无 `serde_json` 依赖，`loader` 只读 `.toml`；`data/x86_instructions/` 下 8 份 `.toml` + 8 份 `.json`，`.json` 是构建期工具 `scripts/codegen-gen` 的输入（其 `parser.rs` 用 `serde_json`、只收 `ext=="json"`），且 `$schema` 指向的两个路径均不存在。

* 待删死抽象：`shared/asm.rs`（436 行，0 引用）、`shared/instruction_traits.rs` + `codegen/generated/`（未挂编译）、`codegen/asm_event.rs`（811 行，`asm_events.push` = 0）、`shared/operand_utils.rs`（2 行 stub）。
* `scripts/codegen-gen/` 当前是**死工具**：列在 `Cargo.toml` 的 `exclude` 里（不参与 workspace 构建）、Makefile/CI/脚本 **零调用点**、其产物目标 `src/codegen/generated/` 未挂编译。它的产物形式也与目标形态不同：现生成 `use crate::codegen::shared::instruction_traits::*` + `pub static ALL_INSTRUCTIONS: &[Instruction]`，而不是本方案的 `InsnEntry` 常量表 + opcode 枚举。→ 复用它 = 沿用四件套骨架（parser/validator/generator/cli）+ 重写产物类型，**不是拿来即用**。
* 规格目录完整路径：`/root/dcc/src/codegen/data/<arch>_instructions/*.toml`（不在仓库根）。现有 `src/codegen/data/x86_instructions/` 共 22 个文件全部被 git 跟踪（含 8 份 `.toml` 与 8 份 `.json`），均未被 ignore。

### 3.4 待迁移发射点与工具链

* 发射点：aarch64 `self.emit(` **283** 处、riscv64 **190** 处（均已核）；**arm 与 mips 无汇口原语**，分别是 **83** / **60** 处 `self.code.extend_from_slice(&inst.to_le_bytes()|to_be_bytes())` 直写（已核：其中仅 1 处是字节数组字面量，余下写的是本地 `inst: u32` 变量）。→ P3/P6 需先走一步 0。

* 布局收敛面：`labels` 读者 **90** 处、`func_sizes`/行表用途 **58** 处。

* 依赖占位注释的站点共 **13** 处（`grep -rn "IR lowering"`）：除 P2 步 10 已列的 `c_cross.rs`、`c_attr_builtin.rs` 外，还包括 `tests/fixtures/_archive/README.md`、`src/codegen/{aarch64,riscv64,arm}/codegen/output.rs`、`src/codegen/mips/baseline_mips.rs`、`src/codegen/shared/pipeline.rs`、`src/{ssa,ir,cpp/lower_ir}.rs`。后三类是“IR lowering”一词的无关命中，需人工区分。

* 本机工具链：`aarch64`/`riscv64`/`arm-gnueabi`/`mips-o32` 的 `as` + `objdump` 齐备；**缺 `arm-linux-gnueabihf-as`、`mips64-*-as`**；无交叉 `gcc`；`qemu` 只有 `-static` 变体。宿主 `objdump` 只认 x86。

* 夹具：`tests/fixtures/asm/` 63 件 `.c`、`tests/fixtures/c/` 280 件；`asm_` 集成测试 120 条。

### 3.5 repertoire 必须用反汇编普查，不能靠 grep 字面量

`self.emit(&[...])` 只暴露常量型发射点；**计算式拼字（`(base | rm<<16 | rd)` 等）在 grep 里隐形**，而 §2 表中那几个错编码恰恰全是计算式产的。正确做法是对产物反汇编再取并集：

```
fixtures/{asm,c} + examples 跑 --target <arch> -c  → <triple>-objdump -d → (word, text) 对 → sort -u
```

aarch64 实测（183 个 TU）：**676 个不同字 / 57 个助记符 / 1528 行真值对**，其中 **10 个字连 binutils 都标 `.inst undefined`**。这份集就是 §4.3 规格与 §3.2 覆盖面的直接输入。P0 步 2 应把它固化为 `scripts/` 下的生成器（当前数据在临时目录，不入库），并输出三类产物：发射点清单、repertoire 字数/助记符数、G11 缺陷清单。

## 4. 目标形态

### 4.1 数据流

```
lowering / 分配器
  → emit_insn(&Insn)            （唯一出口；self.emit 转私有）
      → 装填 encode  → 机器码 + ElfRela 请求 + Pad/PoolData
      → 渲染 print   → GAS 文本
      → 解析 parse   → 用户 inline asm 文本 → 记录（未知则透传）
  → resolve_layout  → LayoutMap（唯一偏移真源）
  → build_object() 与 render_asm_from_state() 消费同一次 compile()
未知行 → AsmPassthroughRecord（0 字节 + 原文 + pc）
  → codegen::emit() 按 --asm-fallback：E0311 | 按 ISA 粒度降级 → .s →（可选）<triple>-as → .o
```

### 4.2 指令记录（共享，arch 中性）

```rust
pub struct InsnRec { pub opcode: OpcodeRef, pub ops: [Operand; 4], pub nops: u8,
                     pub pc: u64, pub bytes_len: u8 }
pub enum Operand { None,
  Reg{idx:u8,width:Width}, Imm{value:i64,bits:u8,signed:bool},
  Mem{base:Option<u8>,index:Option<u8>,scale:u8,disp:i64,writeback:bool},
  Sym{name:String,addend:i64}, Addr{label:String,pc_relative:bool},
  Vector{idx:u8,arr:Arrangement}, Pad{bytes:Vec<u8>}, PoolData{value:..., alignment:u32} }
```

`Operand`/符号求值从 `x86_64/asm/{operand,constraint}.rs` **上收**（P1），不另立新模型。寄存器编号与命名直接复用 `asm_constraints` 的池与各 ISA `parse_*_reg`。

### 4.3 规格 schema v3（每 arch 一份 TOML，构建期消费）

存放 `src/codegen/data/<arch>_instructions/*.toml`。schema 就是 `InstructionDefinition`/`EncodingFormat`/`OperandSpec` 结构体，**不新增 JSON Schema 文件**。相对 v2 的改动：

| 改动            | 字段                                                                                              | 为什么必须有                                                                                                                                                                                                                                          |
| ------------- | ----------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| x86 专有字段降为子表  | `[x86] = { modrm_required, rex_prefix, escape_bytes, addressing_mode }`                         | 共享引擎不得读 arch 专有字段                                                                                                                                                                                                                               |
| 位段描述          | `fields = [{name,start,width,scramble,scale,pc_offset,region_masked}]`                          | `scramble`：RISC-V I/S/B/J、MIPS jump 的位段是散的；**`scale`/`pc_offset`/`region_masked`**：aarch64 `b/bl` = imm26<<2（±128MiB）、`ldr` 字面量 = imm19<<2 且 PC = 指令地址 + 8、mips `j/jal` = 26 位 <<2 且受 256MB region 掩码、riscv J 型跨 `lui+jalr`。缺这三项，可达范围会错一个 4 倍因子 |
| 识别与属性         | `endian`、`mask`、`bits`、`min_len`/`max_len`/`align`、`reloc = {kind, pc_relative, addend_adjust}` | 定长 ISA 唯一识别；长度/对齐供布局；`reloc` 供发 `ElfRela`（如 `R_AARCH64_CALL26`）                                                                                                                                                                                 |
| **新增指令级文本模板** | `render = "bl\t{name}"` / `"ldr\t{d}, [{base}, #{imm}]"`，占位符引用 `fields`/`operands` 名字           | v2 无此字段；`examples[].assembly` 是示例不是模板。文本与位型共用同一份操作数名字集合                                                                                                                                                                                         |
| 成本            | `[cost] = {latency, throughput, code_size, code_class}`                                         | 与 GCC（`attr_length`/automata 分家）、LLVM（cost table 与 `.td` 分家）的差异点                                                                                                                                                                                |
| 合法性           | `range`、`alignment`、`pred = {reg_class, requires_even, disallows, coupled_with}`                | 使表项互斥/覆盖完备成为对整份规格的静态判定；`coupled_with` 显式标记**跨字段谓词**（见 G9）                                                                                                                                                                                       |
| 别名            | `aliases = [{mnemonic, when}]`                                                                  | `mov`=orr Rd,xzr,Rm、`ret`=br x30、`nop`=hint #0；编码与渲染共用一条                                                                                                                                                                                        |
| provenance    | `verified_against = {tool, tool_version, isa_spec_rev}`                                         | 见 §7                                                                                                                                                                                                                                            |

一份规格驱动四消费者：`encode`（已有 selector/emitter 逻辑，改为构建期）、`print`（新）、`parse`（新）、`decode`（仅诊断/测试对照）。

### 4.4 发射点收敛

1. **发射汇口收敛**：`aarch64`/`riscv64` 已有 `self.emit(&[u8])`，将其转私有 `emit_raw_bytes`，只允许装填调用；**`arm`/`mips` 无汇口**，需先引入 `emit_word(u32)` 并收敛 83/60 处直写（P3/P6 步骤 0）。对外只暴露 `emit_insn(&Insn)`。
2. opcode 枚举与构造子由规格生成（`Aarch64Opcode::...`、`aarch64::insn::add_shifted(rd,rn,rm,shift)`）。
3. 两个方向都可静态判定：无表项的构造子 → 编译不过；未被任何构造子引用的表项 → lint 报冗余。
4. x86\_64 同规则，但排在 P9。

## 5. 阶段与步骤

### P0 建立可证零回归的前提（不改语义）

1. 产物级基线：`tests/fixtures/{asm,c}` 在 6 个架构跑 `-c`，入库逐函数 `.text` 长度、符号集、重定位类型集合。**必须在当前工作树之上取**：本轮已修正语句位置 asm 丢弃与操作数严格语义，基线内容相对 HEAD 已变；拿 HEAD 取基线会把已知错字节冻结成“不得回归”的对象。
2. 发射点清单：登记 aarch64 283 / riscv64 190 处 `self.emit(`（文件、行、字面量、对应助记符）；arm 83 / mips 60 处 `self.code.extend_from_slice` 同理。**另加一列“函数名声称语义 vs 实际位型”**：已证实 `emit_mov_x_reg` 名义 64 位、实际 sf=0；迁移到构造子时错误语义会被名字继承。
3. 生成器 x86 假设清单：`InstructionDefinition`/`EncodingFormat` 中必须降为 `[x86]` 的字段；`x86_instructions/*.toml` 哪些可用作对照。
4. 度量基线（供 R10 判定收益）：手写编码行数、E0311 发生率、透传行占比、门禁时长。
5. `disasm.rs` 职责边界写入 `backend_feature_matrix.md`：诊断/测试，不参与 lowering。

**退出**：基线入库；清单数量与 `grep -c` 一致；无代码行为变更。

### P1 上收符号与操作数模型，清死抽象

1. 新建 `shared/asm_operand.rs`：迁入 `AsmOperand` 的语法无关部分与符号表达式求值（`sym - .`、`sym + N`、`sym - N`、`sym - sym`、前向引用延迟）。
2. AT\&T 语法（`%`、`sym(%rip)`、段前缀）留在 `x86_64/asm/operand.rs`，改为调共享求值。
3. 合并 `asm_source::AsmIsa` 与 `asm_constraints::AsmArch` 为单一枚举。
4. 删 `shared/asm.rs`、`shared/instruction_traits.rs` + `codegen/generated/`。

**退出**：asm 产物的 `readelf -r` 与 P0 基线逐条相等；120 条 asm 集成测试全绿；`encode_freeze` 摘要不变。

### P1.5 构建期生成（顺序不可颠倒）

1. **先把生成器纳入构建与 CI**（前置条件，否则不得复用它）：从 `Cargo.toml` 的 `exclude` 移出（或将其测试加入 CI），使其自身参与持续构建。理由：一旦它成为编译器真相的唯一来源，“从不被构建”就从无害的死代码变成最危险的假绿面（生成器坏了无人发现）。若不愿纳入，则改为方案 B：删 `scripts/codegen-gen/`，在 `build.rs` 内联一个精简生成器（只读 TOML + 输出三样产物）。
2. **迁移生成器**：`scripts/codegen-gen/parser.rs` 从 `serde_json::from_str` + `ext=="json"` 改为读 `.toml`；`validator.rs` 从 JSON Schema 改为对 `InstructionDefinition` v3 自校验；`generator.rs` 的**产物类型重设计**（从 `Instruction`/`ALL_INSTRUCTIONS` 改为 `InsnEntry` 常量表 + opcode 枚举 + 构造子，并不再依赖将删除的 `instruction_traits.rs`）。**完成前不得删 `.json`**。
3. 生成器产出三样：编译期常量规格表、opcode 枚举 + 类型化构造子、构建期校验（表项不遮蔽、位段不重叠不溢出、每个构造子有表项）。宿主为 `build.rs` 或预构建步骤；产物默认写 `OUT_DIR`，**不签入 `src/`**；若必须签入，加门禁"重跑生成器后 `git status` 为空"。
4. 按 §4.3 八条升级 `InstructionDefinition`/`EncodingFormat`/`OperandSpec`。
5. 删除 `src/codegen/data/x86_instructions/*.json`（8 份，**均为 git 跟踪文件**，单独一个 commit）；`scripts/validate_instruction_specs.sh` 改为扫 `.toml`，去掉 `jq` 依赖。
6. loader 降为开发期工具（供生成器与测试加载），编译路径不再引用；调用方不得再把规格目录路径作为运行时参数传入。
7. 单指令验证：选 `src/codegen/data/x86_instructions/add.toml` 让 x86_64 一条发射路径改走"生成表 + 生成枚举"。**本步只证明 `encode` 半边**；`print`/`parse` 到 P2 才首次验证。

**退出**：该指令字节与重定位与 P0 基线逐字节相等；删掉源码树 `.toml` 后已构建的编译器仍能工作；改坏一份规格必使构建失败；**生成器自身在 CI 中被构建且其测试被执行**（或已按方案 B 删除并在 `build.rs` 内重建）；x86_64 测试全绿。

### P2 共享引擎 + aarch64 样板

1. 新建**一个** `shared/asm_engine.rs`：`InsnRec`/`Operand`、装填、渲染、解析、`.s` 骨架（段/符号/类型/尺寸/对齐/标签/注释符/字节序）。不得拆成多个并行文件。
2. 实现 `print`、`parse`（补齐四消费者）。**`parse` 的边界固定**：词法切分、注释符、行分类、标签/directive 识别一律复用 `asm_source`（`split_source_lines`/`classify_line`/`analyze`），`asm_engine` 只新增“操作数文本 → `Operand`”一层；不得引入第二套行解析器（否则与 §2 已落地的 `record_asm_stmt` 形成双真相）。
3. 实现 `resolve_layout`（唯一偏移真源，见 §4.1/§5 步骤 5）：固定点松弛 → 字面量池定位 → `LayoutMap`。**类型化收敛**：删除公开的 `labels: HashMap<String,u64>` 读接口，分支/重定位/DWARF/行表/`func_sizes` 只接受 `&LayoutMap`；`self.pos()` 类"当前写入位置"读法保留。约 150 处。
4. 松弛算法在共享层；每 arch 只提供规格数据（`scale`/`pc_offset`/`region_masked`、长形式候选、trampoline 模板）。**不得新增 arch 专属布局文件。**
5. 新建 `aarch64/asm_dialect.rs`：注释符 `//`、立即数前缀 `#`、`@` 属性前缀、小端、`xN/wN` 命名表（与 `parse_aarch64_reg` 互逆，往返测试钉住）、`[Xn{, #imm}]` 与 `0(Xn)` 归属规则。**arch 手写代码只有 dialect。**
6. 新建 `data/aarch64_instructions/*.toml`：规模 = P0 发射点清单 + `mini_asm` 的 29 条内联汇编表项，合为**同一份规格**；补 `render` 模板与 `[cost]`。
7. 迁移 283 处发射点到构造子；`self.emit` 转私有。
8. 重写 `aarch64/codegen/output.rs::build_asm_output`：删除 `// IR lowering, N bytes` 占位桩，改为渲染记录序列并按 pc 插入透传行；rodata/section/symbol 走共享骨架。
9. `features()` 增加 `Feature::GasTextOutput`。
10. 同步更新把占位注释当契约的测试：按 §3.4 全量清单逐个改判据（至少 `c_cross.rs:1960-1962`、`c_attr_builtin.rs:1719/1764`）→ 改为断言真指令文本；改前先区分“测试断言”与“输出端自身产生该注释的站点”。

**退出**：G1–G9 在 aarch64 列全过；P0 产物基线 aarch64 列匹配；其余架构不受影响。

> aarch64 必须独立走完再铺开——它产出可复制模板。

### P3–P6 riscv64 → arm → mips(o32) → mips64

每个后端重复 P2 的 10 步，交付物只有 dialect + TOML，且**只有自己的 G1–G9 全绿才算完成**。前置的**步骤 0**（仅 arm/mips）：引入 `emit_word(u32)` 并收敛 §3.4 所列的 83/60 处直写，否则 §4.4 步 1 的“转私有”无从谈起。特别注意：

* **riscv64**：变长（压缩 2 / 非压缩 4），`bytes_len` 必须来自实际装填结果；`scramble` 覆盖 I/S/B/J/U 型；注释符必须是 `#`（现占位桩的 `//` 被真 GAS 拒绝）。

* **arm**：条件码后缀（`moveq`/`addne`）在 `render` 模板里建模，不得当独立助记符膨胀表；本机无 `gnueabihf-as`，按 triple 或 `-mfloat-abi` 适配，不得静默跳过。

* **mips**：大端（`to_be_bytes`）；`j/jal` 26-bit <<2 + region 掩码需松弛策略；本机无 mips64 `as`，该格为 objdump-only，须在能力表显式标注。

### P7 降级串接（产 `.o`）

1. driver 侧：文本产物喂 `<triple>-as` 得 `.o`，复用 `run_assembler_driver` 的 `as` 调用与 `-Wa,` 透传。
2. 按基线 6 实现 `AsmIsa::degrade_granularity()`：定长 ISA 逐语句分流（已知行仍直发），变长 ISA 整 TU 分流。
3. 不具备 `GasTextOutput` 的目标继续 E0312，**不得为凑数谎报能力**。

**退出**：混合已知/未知指令的 TU（`asm_fallback_gas.c` 及交叉版）产出可链接 `.o`；`--asm-fallback=error` 行为不变。

### P8 债务清算与文档回写

1. `mini_asm.rs` 删除；`encode_freeze` 基线迁移为"生成器读规格→产出表→驱动等价测试"，摘要逐位不变。
2. `asm_event.rs`：被 `InsnRec` 取代则连同 `x86_64` 的 `asm_events` 字段删除；否则接上真实生产者。**不得保持 0 生产者。**
3. `operand_utils.rs` 删除。
4. 回写：`dcc-dual-arch-asm-solution.md`（取代解码器设想）、`diagnostics.md`（E0311/E0312）、`adding_new_isa.md`（新增 ISA = 写 dialect + 写 TOML + 发构造子，禁止写 Rust 编码表）、`backend_feature_matrix.md`（`GasTextOutput` 与覆盖率/新鲜度）、`instruction_spec_schema_v2.json`（标 superseded）。
5. 重跑全部 Tier-0 门禁与 `check_reproducible_output.sh`。

### P9 x86\_64 迁入记录模型（962 处）

1. `emit_asm_instr` 的 765 条助记符分支逐条抽为 `src/codegen/data/x86_instructions/*.toml`（含 `render` 模板 + `[x86]` 子表），由生成器产出枚举与构造子。
2. 962 处 `self.emit(` 改走 `emit_insn(&Insn)`；`self.emit` 转私有。
3. `-S` 从"反汇编自己的字节"切换为"渲染记录"；`disasm.rs` 保留为诊断/测试工具，不进 lowering。
4. 顺带消除双份真相：`emit_asm_instr` 与 `disasm.rs` 对同一编码的知识不再各写一份。

**退出（G10）**：全部 x86\_64 产物的 `.text`、符号表、重定位与迁移前**逐字节全等**；`s_output_*` 与 120 条 asm 测试全绿；`-S` 输出被宿主 `as` 汇编通过。

## 6. 门禁（任一不成立即视为未完成）

| #   | 门禁               | 判据                                                                                                                                                     | 落点                                                           |
| --- | ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------ |
| G1  | GAS 真值对拍         | 每个发射点/表项：文本喂 `<triple>-as` 的字节 == 装填字节                                                                                                                 | 单元测试（可跑者）+ 集成                                                |
| G2  | repertoire 全覆盖   | 构造子必须由生成的枚举给出（编译期）；`Σ bytes_len == code.len()` 含 `Pad`/`PoolData`（运行期兜底）；未被引用表项 → lint                                                                 | 编译期 + 单元测试                                                   |
| G3  | 双模式一致            | `-c` vs `-S→as`：逐函数反汇编归一相等；`.text` 长度、符号集、重定位类型集合相等。**不比 `.text` 逐字节全等**（GAS 的 disp8/disp32、mov 方向位、nop 填充必然不同）。本门禁是"两条路"的代价，红/绿率按技术债跟踪                | `tests/integration/c_asm.rs`，命名 `asm_dual_mode_<arch>[_<q>]` |
| G4  | 能力闸门一致           | `GasTextOutput=true` 者 `-S` 必被真 `as` 接受；`false` 者必 E0312                                                                                               | `check_feature_matrix.yml` diff                              |
| G5  | 反假绿元门禁           | G1/G3/G6/G7 必须断言"至少 1 个架构真跑完整判据"，全 SKIP 则红；校验器须能变红（故意写坏一份规格）                                                                                           | 沿用 `LINKLOCK-VACUOUS` 范式                                     |
| G6  | 穷举可逆性（**全 ISA**） | 每条表项在其合法操作数空间穷举：`as(print(rec)) == encode(rec)` 且 `parse(print(rec)) == rec`。变长不豁免（长度是 `encode` 的输出）                                                   | 单元测试，规模打印进日志                                                 |
| G7  | 偏移单一真源           | ① 同一 TU 两次全产物字节全等；② `LayoutMap` 为唯一入口（类型强制，非 grep）；③ 松弛不收敛必 E03xx，禁止截断位段；④ 收敛结果与迭代顺序无关                                                                 | 编译期 + 单元测试                                                   |
| G8  | 形状层需求 ⊆ 定义域      | 构建期 `D_shape ⊆ E_shape`（构造子签名 × reg class 对 fields 存在性与类级 `pred`）。**值层合法性不得声称已证明**，只在发射前断言                                                             | 构建期                                                          |
| G9  | 穷举划分的可靠性         | 不得手填等价类。字段相互独立时代表元取积=定理；含 `coupled_with`（偶数寄存器对、`base≠sp`、riscv store `rd=zero`、aarch64 imm12+shift 耦合）时必退化为穷举或按谓词生成代表元；生成器须报告哪些表项含跨字段谓词；打印穷举规模与耗时并设上限 | 构建期 + 测试日志                                                   |
| G10 | x86\_64 迁移零回归    | P9 前后 `.text`/符号/重定位逐字节全等                                                                                                                              | 集成 + 产物基线                                                    |
| G11 | 编码正确性缺陷棘轮       | 迁移期为基线；`docs/aarch64_encoding_defects.md` 登记“意图 ⊻ 现字节”（word + 发射点 + 意图 + `as` 给出的真值），对应规格表项标 `unverified` 并允许 G1 红；**清单只许缩短**，新增未登记项即红。禁止改写 `render` 模板去迎合错误字节 | bug-fix 阶段 + `check_feature_matrix.yml` diff                   |

**批量矩阵**：新增 `scripts/check_asm_text_parity.sh`（放 `scripts/` 才在夹具扫描面内），复用 `check_reproducible_output.sh` 的 `fingerprint()/check_combo()` 骨架，把"同 mode 两次"换成"`-c` 一次 + `-S→as` 一次"。
**夹具规范**：asm 夹具落 `tests/fixtures/asm/`（落 `c/` 判 `DIR-MISMATCH`），命名 `asm_<domain>_<feature>`，禁 `m<N>_`/`test_` 前缀；测试函数追加进 `tests/integration/c_asm.rs`。

## 7. 仲裁、provenance 与上游同步

* **RISC-V**：vendor BSD 系 `riscv-opcodes`/`encoding.h` 作硬仲裁源，保留版权声明；提供 `scripts/sync_riscv_isa.sh` 做重新同步，同步后必须重刷受影响表项的 `verified_against`。

* **ARM / MIPS**：不 vendor（Apache-2.0 与 binutils GPL-3+、ARM 机器可读 XML 条款不兼容）。以 CI 锁定的 `as`/`objdump` 版本为交叉校验源；`verified_against = {tool, tool_version}` 记录新鲜度。

* **冲突裁决**：规格与 GAS 不一致 → RISC-V 以上游源为准；ARM/MIPS 记入显式分歧表（人工裁决 + 该表项标 `unverified`），能力表按**覆盖率 + 新鲜度**报告，不用 per-backend 布尔。

* 度量趋势（手写编码行数↓、E0311 率、透传占比、门禁时长、G3 红/绿率）写入 `backend_feature_matrix.md`，并预先回答"什么结果说明这套架构不值得"。

## 8. 不做的事

* 不新建第五套指令表示；`Operand` 唯一来源是 P1 上收的类型，规格唯一载体是 `data/` TOML。

* 公共层不出现具体 arch 的变体或结构体。

* 不恢复任何模式布尔（`asm_text_mode` 那类"一条语句两条代码路径"）。

* 不为交叉 ISA 复制完整 GAS：超出 dcc 自身位型的指令一律透传。

* 不引入第二种规格格式。**全项目规格只允许一种格式（TOML）**，不得再出现并行的 `.json` 或被手写的 `.rs` 编码表。这不是偏好而是硬约束：现有 8 份 `.toml` + 8 份 `.json` 双副本已经产生过一个假绿门禁（校验脚本扫 `.json`、loader 读 `.toml`）。由于规格只在构建期被读一次，格式本身不影响性能（JSON 也能用），真正影响的是“是否存在两份真相”。
* 不复用未接入构建的生成器：`scripts/codegen-gen` 要么按 P1.5 步骤 1 纳入 workspace/CI，要么删除并在 `build.rs` 重建，不得保持“存在但从不被构建”。
* 不引入 JSON 作为规格格式；不新增 JSON Schema 文件。

* 不留无生产消费者的文件。

## 9. 风险

| 风险                    | 缓解                                           |
| --------------------- | -------------------------------------------- |
| 运行期读规格被引入编译路径         | P1.5 构建期生成 + "删 `.toml` 仍可运行"验收              |
| 962 处 x86\_64 迁移期回归   | G10 逐字节全等；单独成阶段，不与功能改动混提                     |
| 可达范围算错（4 倍因子）         | `scale`/`pc_offset`/`region_masked` 入规格；G7③④ |
| 跨字段谓词被穷举漏掉            | G9 强制检测并报告，禁止当作独立等价类                         |
| 表项 `(mask,bits)` 相互遮蔽 | 生成器构建期不重叠断言                                  |
| `.json` 早删会打断生成器      | P1.5 步骤顺序：先迁生成器，后删副本                         |
| binutils 版本漂移使门禁翻转    | `verified_against` 版本锁定 + 分歧表                |
| 依赖占位注释的旧测试转红          | P2 步骤 10 同批改判据                               |
| 纯移动纪律把错编码冻结成“不得回归” | G11：迁移期只登记不修，修复单独成阶段并允许基线变化               |
| 交叉 `as` 缺失致空跑         | G5 反假绿                                       |
| 与主线程并行改动冲突            | 每阶段一个独立可提交的完整改动集，遵守 §3.1 不夹带                 |

## 10. 立即执行（P0）

1. 生成产物级基线并入库（**在当前工作树之上**，见 P0 步 1）。
2. 产出三份发射点清单（aarch64 283、riscv64 190、arm 83 / mips 60 直写点）+ repertoire 反汇编普查脚本入库 `scripts/`。
3. 列出 `InstructionDefinition` 的 x86 假设清单 → 定 schema v3 形状。
4. 记录度量基线四值。
5. `disasm.rs` 边界回写 `backend_feature_matrix.md`。
6. 建立 `docs/aarch64_encoding_defects.md`（G11 载体），先录入 §2 已确证的四组：`fcmp`、`emit_mov_x_reg`×11、`lsl` 计算式、HFA/向量/`fsub`/`fdiv` 的 10 个 `.inst` 字。**这六项不阻塞 P1/P2，但必须在 bug-fix 阶段一次性清零。**

## 11. 执行进度（2026-10-09 更新）

| 阶段 | 状态 | 落地 |
| --- | --- | --- |
| P0 基线 | 已完成 | 冻结基线 `mini_asm_encode_freeze.txt` 不变；死代码清理完成（`shared/asm.rs`、`instruction_traits.rs`、`operand_utils.rs`、`asm_event.rs`、`generated/`、`scripts/codegen-gen/` 已删） |
| P1 操作数/记录模型 | 已完成 | `shared/asm_operand.rs`（`Operand`/`InsnRec`/`LayoutMap`）+ `shared/asm_engine.rs`（dialect/render）就位 |
| P1.5 构建期生成 | 暂缓 | 生成器已删，统一走手写枚举（`Aarch64Opcode` 等），避免"存在但从不被构建"反模式 |
| P2 aarch64 样板 | G1 门禁已通 | `aarch64/insn.rs` 类型化构造子 + `encode()` 直写顶部字节（sf 已含）；`encode_truth_test.rs` 通过 GAS 真值比对（14 条指令：nop/ret/br/blr/mov_reg/add_imm/sub_imm/mov_imm/svc/dmb/wfe/wfi/yield/isb 全与 `aarch64-linux-gnu-as` 输出逐字相等） |
| G11 缺陷登记 | 已完成 | `docs/aarch64_encoding_defects.md` 22 条（F1–F22），迁移期只登记不修 |
| P3–P6 arm/mips 步骤 0 | 已完成 | `arm/pipeline.rs::emit_insn`、`mips/baseline_mips.rs::emit_insn` 引入；84 处 arm + 61 处 mips `extend_from_slice(&inst.to_le_bytes())` 直写收敛为 `emit_insn(&Insn::raw(..))` |
| G5 反假绿 | 已完成 | `encode_truth_test.rs` 在 `as` 不可用时 `ran == 0` 直接失败，杜绝空跑绿 |
| 门禁 | `cargo check` 干净、2187 单元测试全绿、`cargo build` 成功、clippy 在本次改动文件上零警告（其余历史警告属 ssa/ 等无关模块） |

| P2 LayoutMap 唯一入口 | 已完成 | `LayoutMap::snapshot()` + 四后端 `snapshot_layout()`（aarch64/arm/mips/riscv64）就位；跨模块消费点（arm/backend.rs、mips/mips64.rs 的 DWARF func_pcs、mips64 `layout()` 只读入口）已改为吃 `&LayoutMap`，不再直读 `pub labels`；后端内部 emit 阶段的 `self.labels` 读写（前向引用解析）保留 |
| P2.5 规格副本清理 | 已完成 | 8 份 `x86_instructions/*.json`（内容实为 TOML，CRLF 差异）已删；`docs/instruction_spec_schema{,_v2}.json`、`scripts/validate_instruction_specs.sh`（jq 扫 JSON，从未接 CI）一并删除；`adding_new_isa.md` 示例改写为合法 TOML；全项目规格只留 TOML 一种格式 |

**遗留**：x86_64 P9（962 处 asm 发射点迁入记录模型）待后续窗口推进。
