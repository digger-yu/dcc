# DCC Compiler Architecture Design

**版本**: 3.0 - Advanced Modular Architecture (Design > Implementation)  
**日期**: 2024-09-23  
**项目**: DCC Multi-ISA Backend Extension - Phase C  

---

## 🎯 Core Philosophy

### Design Leads, Implementation Follows

**Key Principle:**  
The architecture design should be **ahead of implementation**, providing a complete blueprint for all backends regardless of their current completion status.

> "设计是设计，实现是实现，不完整的可以后期补齐，但设计要先进"

---

## 📊 Strategic Vision

### Universal Backend Pattern

```
┌─────────────────────────────────────────────────────────────────┐
│                     Unified Architecture Pattern                │
│                    (Applies to ALL Backends Equally)            │
└───────────────────────────┬─────────────────────────────────────┘
                            │
         ┌──────────────────┴──────────────────┐
         │                                     │
┌────────▼────────┐                   ┌───────▼────────┐
│ x86_64 Backend  │                   │ AArch64 Backend│
│ [Tier-1]        │                   │ [Tier-1]       │
│ Production      │                   │ Production     │
│ Ready           │                   │ Ready          │
└────────┬────────┘                   └───────┬────────┘
         │                                     │
         └──────────────┬──────────────────────┘
                        │
         ┌──────────────▼──────────────────────┐
         │           SHARED LAYER               │
         │   (codegen::shared/* modules)       │
         │   ────────────────────────────      │
         │   • Backend Trait Definition        │
         │   • Registry System                 │
         │   • Feature Set Management          │
         │   • ELF Object Generation           │
         │   • Cross-ISA Utilities             │
         └──────────────┬──────────────────────┘
                        │
         ┌──────────────┼──────────────────────┐
         │              │                      │
┌────────▼────┐ ┌──────▼──────┐    ┌─────────▼─────────┐
│RISC-V 64    │ │ MIPS/MIPS64 │    │     ARM           │
│[Tier-1]     │ │[Tier-1]     │    │   [Tier-2]        │
│Production   │ │Production   │    │ Development       │
│Ready        │ │Ready        │    │ Track             │
└─────────────┘ └─────────────┘    └───────────────────┘
```

### Important Note:
**Even though ARM is currently in development**, its **design is fully specified** and follows the same pattern as other backends. The difference is purely in implementation maturity, not architectural completeness.

---

## 🏗️ Complete Backend Template (Advanced Design)

### Every Backend MUST Have This Structure

```
src/codegen/{backend}/
├── mod.rs                              # Module declaration & exports
│   ├── Backend trait implementation
│   ├── Public API exports
│   └── Integration with registry system
│
├── registers/                          # ⭐ Register Abstraction Layer
│   ├── mod.rs                          # Register definitions for THIS ISA
│   ├── constants.rs                    # Named register constants
│   ├── encoding.rs                     # Register encoding rules
│   └── allocator.rs                    # Register allocation helpers
│
├── abi/                                # ⭐ ABI/Calling Convention Layer
│   ├── mod.rs                          # Public ABI interface
│   ├── calling_convention.rs           # Parameter passing rules
│   ├── stack_layout.rs                 # Stack frame organization
│   ├── prologue_epilogue.rs            # Function entry/exit code
│   └── varargs.rs                      # Variadic argument handling
│
├── codegen/                            # ⭐ Core Code Generation
│   ├── mod.rs                          # Main codegen dispatcher
│   ├── compile.rs                      # Top-level compilation flow
│   ├── functions.rs                    # Function-level codegen
│   ├── globals.rs                      # Global variable emission
│   ├── control_flow.rs                 # Branches, loops, switches
│   └── blocks.rs                       # Basic block processing
│
├── emit/                               # ⭐ Statement/Expression Emission
│   ├── mod.rs                          # Emitter coordination
│   ├── stmt.rs                         # Statement-level emission
│   ├── expr/                           # Expression submodules
│   │   ├── binary.rs                   # Binary operations
│   │   ├── unary.rs                    # Unary operations
│   │   ├── call.rs                     # Function calls
│   │   ├── lvalue.rs                   # L-value address computation
│   │   ├── literal.rs                  # Literal value emission
│   │   ├── compound.rs                 # Compound assignments
│   │   ├── cast.rs                     # Type casting
│   │   ├── init.rs                     # Initialization expressions
│   │   ├── va_arg.rs                   # va_list access
│   │   └── inline_asm.rs               # Inline assembly handling
│   └── asm/                            # Assembly-specific code
│       ├── emitter.rs                  # Machine code emitter
│       ├── planner.rs                  # Instruction scheduling
│       └── expansion.rs                # Macro/template expansion
│
├── helpers/                            # ⭐ Utility Functions
│   ├── mod.rs                          # Utility exports
│   ├── encoding.rs                     # Instruction encoding utilities
│   ├── addressing_modes.rs             # Addressing mode helpers
│   └── label_generator.rs              # Unique label generation
│
└── special/                            # ⭐ Special Features Layer
    ├── mod.rs                          # Feature exports
    ├── tls.rs                          # Thread-local storage support
    ├── sanitizer/                      # Sanitizer runtime support
    │   ├── asan.rs                     # AddressSanitizer
    │   ├── hwasan.rs                   # Hardware ASAN
    │   ├── msan.rs                     # MemorySanitizer
    │   └── ubsan.rs                    # UndefinedBehaviorSanitizer
    ├── mte.rs                          # Memory Tagging Extension
    └── vector_extensions.rs            # SIMD/vector extensions
```

### Key Design Principles:

1. **Uniform Structure**  
   Every backend follows EXACTLY the same directory structure, regardless of completion status.

2. **Interface Completeness**  
   All interfaces must be defined upfront, even if some are temporary stubs.

3. **Progressive Implementation**  
   Start with minimal viable implementation, then progressively enhance each module.

4. **Layered Abstraction**  
   Clear separation between:
   - Register management
   - ABI/calling convention
   - Core codegen logic
   - Concrete expression/statement emission
   - Utility functions
   - Special features (TLS, sanitizers, etc.)

---

## 🔧 Shared Infrastructure (Universal Dependencies)

### All Backends Depend on These Common Modules:

```
src/codegen/shared/
├── mod.rs                          # Shared module exports
├── backend.rs                      # Unified Backend trait definition
├── registry.rs                     # Backend registration/discovery
├── features.rs                     # FeatureSet capability system
├── types.rs                        # Shared type definitions
├── elf.rs                          # ELF object file generator
├── coff.rs                         # COFF/PE format support (Windows)
├── asm_constraints.rs              # Cross-ISA inline asm constraint parsing
├── mini_asm.rs                     # Non-x86 inline assembly encoder
├── control_flow.rs                 # Generic control flow handlers
├── expressions.rs                  # Shared expression evaluation framework
├── instruction_encoding.rs         # Data-driven instruction encodings
├── register_allocator.rs           # Generic register allocation algorithms
└── optimization_passes.rs          # Common optimization passes
```

---

## 🧠 SSA 优化管线 (src/ssa/ —— 目录 + mod.rs 双层结构)

入口 `opt.rs::run_ssa_passes`: 把结构化函数 `AST → SSA → pass → AST` 转一圈再回写
`f.body`。

### 模块布局（与 codegen 侧同一套双层结构规范）

`src/ssa/mod.rs` 只有声明、再导出与「地图注释」，实现一律在子文件里；外部调用方继续走
`crate::ssa::*`（由 `mod.rs` 的 `pub use` 维持），**不需要知道内容在哪个文件**。
分层大致是 `ir → cfg → structure → build/lower → loop_exit/inline_temps → rebuild/expr_rebuild`，
但**不是严格单向**，已知两条反向边：`ir::inst_unremovable`/`inst_written_bases` 调
`rebuild::inst_has_side_effect`，`loop_exit` 调 `rebuild::{is_pass_hint_inst, is_loop_exit_pure_return}`。
它们是为了「判据只有一份」而存在的共享，**不是层次倒置**，拆模块时不得当作可断开的边。

```
src/ssa/
├── mod.rs              103 行：13 条 pub mod + 14 条 pub use 再导出 + 地图注释
│                       （不放实现；`pub use` 只列**目录外真有消费者**的名字，
│                       其余项降为 `pub(super)` + 仅 `ssa` 内部可见的再导出）
├── ir.rs               IR 数据形态与属性判据：SsaVar / Operand / InstKind / Inst /
│                       Terminator / BasicBlock / Phi / OrderedNameSet / Function /
│                       collect_uses_* / rename_*
├── cfg.rs              CFG 与支配：term_succs / collect_all_preds / compute_dominators /
│                       compute_ipdoms / is_back_edge / reverse_postorder / refresh_cfg /
│                       find_loop_body{,_multi} / ordered_block_names / dangling_refs
├── structure.rs        进 SSA **之前**的 AST 侧政策：is_structured* 准入判据 +
│                       lower_switches*（switch → if-chain）
├── build.rs            AST → SSA 唯一入口：Builder + build_from_func / build_ssa
├── lower.rs            Builder 调用的降级辅助：lower_lvalue / collect_local_decls* /
│                       insert_loop_phis / insert_label_phis
├── loop_exit.rs        多出口循环的出口归属 —— 判据(has_multi_exit_cycle) 与发射器同源
├── inline_temps.rs     合成临时值的内联窗口：plan_inlined_temps / inline_window_safe
├── rebuild.rs          SSA → AST 的**控制流**重建：rebuild_block / RebuildCtx / rebuild_bb /
│                       rebuild_cycle / emit_rebuild_inst / walk_to_join / branch_join
├── expr_rebuild.rs     SSA → AST 的**表达式**重建与类型推导：inst_to_expr{,_resolved} /
│                       operand_to_expr / infer_binop_type / store_pointee_ty / addr_of_ty
├── dump.rs             dump_function —— `DCC_SSA_IR_DUMP` / `DCC_DUMP_CPP_AST` / `DCC_PIPE_DUMP`
│                       三条通道共用的 IR 读数（只读不写，不进产物字节）
├── passes.rs           M16 优化 pass 集合（原 src/ssa_passes.rs）
├── pipelining.rs       P16 软件流水（原 src/ssa_pipelining.rs）
├── regalloc.rs         P12 SSA 层寄存器分配（原 src/ssa_regalloc.rs）
├── tests.rs            跨切用例（构造 → 跑 pass → 重建查产物形态），不拆散
└── unary_rebuild_ty_tests.rs  一元指令重建的类型回归
```

**单一真源的落点**（改判据前先确认只有一处）：SSA 化准入与降级政策只在 `structure.rs` /
`build.rs`；出口边归属只在 `loop_exit.rs`（实测 `rebuild.rs` 共 7 个调用点：5 处 `loop_exit_owner` + 2 处 `loop_exit_plan`，均必须指向
`loop_exit.rs` 的唯一实现）；表达式类型推导只在 `expr_rebuild.rs`；诊断文案在 `dump.rs` /
`pipelining.rs::dump_plan`。

与 SSA 无关、但名字曾带 `ssa_` 前缀的一个模块已正名为 **`src/ast_gvn.rs`**（它在 AST 上
做 GVN/CSE/拷贝传播，从不碰 SSA IR；SSA 上的值编号在 `src/ssa/passes.rs::gvn`）。

**准入闸门**（三条，缺一不可）：

| 闸门 | 作用 |
|---|---|
| `is_structured(body)` | 拒 goto / label / switch / asm goto 等非结构化控制流 |
| `func_has_loop(f)` | 含 while/for/do 的函数不进 SSA —— `rebuild_cycle` 尚未正确（且该判据必须**穷举**所有可含语句的形态，包括 `Stmt::Switch` 的 case 体、`Stmt::Label`、语句表达式 `({...})`、嵌套函数体；`lower_switches_in_func` 在闸门**之后**才展开 switch，漏一个形态就等于让循环带着走进 SSA） |
| `func_has_unsafe_ssa_pattern` / volatile 全局 | static 局部、带初值的数组、引用 volatile 全局的函数 |

### 当前 pass 调度 (`run_with_opts`, 仅 -O2+ 进入)

| # | pass | 对产物的影响 | 备注 |
|---|---|---|---|
| P2 | `strength_reduction` | 有 | 乘/除 2 幂 → 移位（按 dest 宽度与符号定宽） |
| P3 | `ivop` | 有 | 归纳变量重写 |
| P10 | `tail_call_optimization` | 有 | 自递归尾调用 → Jump(entry) |
| P14 | `speculative_execution` | 有 | 分支条件的**依赖闭包**上提（不得按字典序重排） |
| P17 | `gvn` | 有 | 全局值编号：整型同形计算跳块归一（`in[BB] = ⋂ out[pred]`）；内存读与浮点运算只块内复用；phi 退化与同块合并 |
| P18 | `dead_store_elim` | 有 | 死存储消除：局部覆盖 + 单入链全局；volatile / 全局 / 被取址且逃逸的写一律保留 |
| P19 | `jump_threading` | 有 | 常量分支折叠 + 空跳板链合并 + 不可达块剔除 + **CFG 缓存与 phi 行修补** |
| P1 | `dead_inst_elim` / `dead_bb_elim` | 有 | GVN/穿线后的清理；**不得删具名变量的赋值**（重建后那个栈槽就永不写） |
| P5 | `instruction_scheduling` | 有 (-O3) | 块内 list scheduling，依赖图驱动 |
| P12b | `ubsan_ir_instrument` | 部分 | guard 已插；“重建时转 `__ubsan_handle_*`” 的说法开之前并不存在 |
| P4/P13 | `alias_analysis_*` / `alias_set` | 无 | 结果被丢弃，仍待接入 P18/P20 |
| P6/P7 | `pgo` / `auto_parallelization` | 无 | 以 `__dcc_*` hint 形式实现，重建时被丢弃（hint 本身在 AST 层由 `parallelize_block` 打标） |
| P16 | 软件流水 | 分析层已完成 | [src/ssa/pipelining.rs](file:///root/dcc/src/ssa/pipelining.rs)：循环识别（单 latch + 直线链区域 + 常量 trip）、依赖图（带距离/延迟，含内存定序与基名塌缩序边）、RecMII/ResMII、epi-cyclic 模调度、`prologue/kernel/epilogue` 三段切分（`InstancePlan::verify` 逐元素比对绝对时间序）、单一闸门 + `rejections` 诊断。**仍不参与编译产物**：把计划应用到 IR（节点重命名 + 三段块 + 一级轮转 phi）未做，见 `docs/ssa-pass-upgrade-plan.md` §7.1 |

**已删除的伪实现**（它们当时都报告 `changed = true` 却对机器码零影响，其中两条还会造成运行时错）：
`function_attribute_propagation`（假 hint 造成 SEGV）、`register_allocation`（全局栈槽模型下不属本层）、
`software_pipelining`（空函数体）、`loop_tiling` / `loop_interchange`（把一条指令原样复制一遍，破坏单赋值）。
AST 层的同名伪流水 `loop_opt::software_pipeline_block` 也已删除：它只往循环前插一条
`#pragma GCC pipelined` 的空 asm（后端当注释，实测 -O3 汇编 0 次出现），不产生任何流水变换。
理由逐条写在 `src/ssa/passes.rs` 对应小节。

### 本层三条硬约束

1. **CFG 只能从终结符读**：`bb.preds` / `bb.succs` 是构造期缓存，改过 terminator 即脏；
   只允许 `term_succs` / `collect_all_preds`，并在 pass 末尾 `refresh_cfg_caches`。
2. **遍历序必须确定**：所有块级循环走 `ordered_block_names` / RPO，不得用 `blocks.values()`
   （HashMap 随机序会直接泄入产物字节，破坏可复现性）。
3. **返回值必须是真事**：`-> bool` 意味着“IR 真的改了”，驱动定点迭代；只插元数据就返回
   true 会把调度器骗成无限循环。

### Pass 级开关（风险兜底）

`DCC_SSA_OFF=dse,gvn,jt,die,dbe` 可逐个关掉本轮新增的 pass，用于定位回归而不必重新编译。
默认全部开启；该开关只用于诊断，**不得作为错漏的长期规避手段**。

## 📋 Implementation Phases with Design-First Approach

### Phase 1: Foundation (Weeks 1-2) - ✅ Done

**Focus: Shared infrastructure only**
- ✅ Create `shared/` module structure
- ✅ Define unified `Backend` trait
- ✅ Implement registry system
- ✅ Add feature capability system

**Deliverable:** Working shared layer that ANY backend can implement

---

### Phase 2: Complete Backend Skeletons (Weeks 3-4) - PENDING

**Focus: Empty skeletons for ALL backends following design**
- ✅ Define complete directory structure for ARM
- ✅ Add empty stub implementations for all modules
- ✅ Ensure compile-time integration with registry
- ✅ Verify all interfaces match design specs

**Important:** Even incomplete backends have FULL design!

**Example: ARM Backend Skeleton**
```rust
// src/codegen/arm/mod.rs
mod backend;           // Implemented later
mod registers;         // Currently stub, implements R0-R15 + FP+LR+SP
mod abi;               // Currently stub, implements AAPCS
mod codegen;           // Currently stub, implements core functions
mod emit;              // Currently stub, implements statement/expression emission
mod helpers;           // Currently stub, utility functions
mod special;           // Currently stub, TLS/sanitizer stubs

// All these directories exist and follow the exact same pattern as x86_64, AArch64, etc.
// Only IMPLEMENTATION differs, NOT DESIGN
```

---

### Phase 3: Progressive Enhancement (Weeks 5-8) - PENDING

**Focus: Fill in implementations gradually**

#### Subphase 3a: Priority Backends
- x86_64 → already complete ✅
- AArch64 → already complete ✅
- RISC-V 64 → enhancement needed
- MIPS/MIPS64 → enhancement needed

#### Subphase 3b: ARM Backend
- Week 5: Implement `registers/` layer
- Week 6: Implement `abi/` layer  
- Week 7: Implement basic `codegen/` layer
- Week 8: Implement `emit/` layer

**Strategy:** Each layer starts as functional stub, gets progressively enhanced

---

### Phase 4: Refactoring & Duplication Elimination (Weeks 9-10)

**Focus: Apply data-driven refactoring across ALL backends**
- Identify common patterns
- Extract shared logic to `shared/` modules
- Replace hard-coded logic with data-driven configurations
- Achieve 60%+ code reduction target

---

### Phase 5: Optimization & Polish (Weeks 11-12)

**Focus: Performance tuning and final polish**
- Profile all backends
- Optimize hot paths
- Enhance error messages
- Document best practices

---

## 💡 Design Philosophy Highlights

### 1. Complete Blueprint First

**Before any implementation:**
- All module boundaries defined
- All interfaces specified
- All public APIs documented
- Directory structure created for EVERY backend

**After specification:**
- Some backends may have minimal implementations
- Some may have partial implementations
- Some may be fully implemented
- But **ALL have complete design**

### 2. Consistent Interface Contract

Every backend implements the SAME `Backend` trait:
```rust
pub trait Backend {
    fn arch(&self) -> Arch;
    fn features(&self) -> FeatureSet;
    fn compile(&mut self, input: &CompileInput<'_>) -> Result<CommonImage, Vec<Diagnostic>>;
    fn render_asm(&mut self, input: &CompileInput<'_>) -> Result<Vec<u8>, Vec<Diagnostic>>;
    fn build_object(&mut self) -> Result<Vec<u8>, Vec<Diagnostic>>;
}
```

This contract exists **regardless of implementation quality**.

### 3. Progression Path

```
┌─────────────────────────────────────────┐
│ Stage 1: Design Complete                │
│ All modules exist as empty/fill-in shells │
└─────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────┐
│ Stage 2: Minimal Viable Implementation  │
│ Functional but basic per-module         │
└─────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────┐
│ Stage 3: Enhanced Implementation        │
│ Full feature set, optimized             │
└─────────────────────────────────────────┘
                    ↓
┌─────────────────────────────────────────┐
│ Stage 4: Refactored & Optimized         │
│ Data-driven, deduplicated, polished     │
└─────────────────────────────────────────┘
```

---

## 🎯 Success Metrics

| Metric | Target | Measurement Method |
|--------|--------|-------------------|
| **Design Completeness** | 100% | All backends have full spec |
| **Implementation Coverage** | >95% | By end of Phase 3 |
| **Code Reduction** | 60%+ | By end of Phase 4 |
| **New ISA Integration Time** | < 2 weeks | Measured by template reuse |
| **Consistency Score** | 1.0 (Perfect) | Automated consistency checks |

---

## 📝 Documentation Strategy

### For Each Backend:

**Design Documents (Must Exist):**
- `ARCHITECTURE.md` - High-level design overview
- `MODULES.md` - Detailed module specifications
- `ABI.md` - Calling convention documentation
- `REGISTER_SET.md` - Register usage rules
- `INSTRUCTION_TABLES.md` - Data-driven instruction mappings

**Implementation Files:**
- Can start as stubs
- Can be incomplete
- Must follow the design exactly

---

## 🚀 Migration Strategy

### Step-by-Step Process:

1. **Define Design** ← Start here!
   - Specify all modules
   - Define all interfaces
   - Document all behaviors
   - Create templates

2. **Create Skeletons**
   - Empty directories
   - Stub implementations
   - Compile-only code

3. **Implement Layer by Layer**
   - Registers first
   - Then ABI
   - Then core codegen
   - Then emitters
   - Finally helpers/special

4. **Enhance Gradually**
   - From stubs to functional
   - From functional to complete
   - From complete to optimized

---

## 🎓 Lessons from Design-First Approach

### What Works Well:
- ✅ Clear expectations from day one
- ✅ Parallel development possible
- ✅ No surprises during implementation
- ✅ Easy to identify gaps
- ✅ Consistent quality across ISAs

### Challenges:
- ⚠️ Requires upfront thinking
- ⚠️ More initial documentation effort
- ⚠️ Need discipline to follow design
- ⚠️ May require rethinking designs

### Best Practices:
- Use RFC process for major design changes
- Keep design documents versioned
- Automate consistency validation
- Regular design reviews

---

## 📌 Key Takeaway

> **"The architecture design exists independently of implementation progress."**

Whether a backend is:
- Production-ready (x86_64, AArch64)
- Nearly complete (RISC-V, MIPS)
- Still developing (ARM)

**They all share the same complete design blueprint.** The difference is purely in implementation maturity, not architectural vision.

This ensures:
- Future backends can be added easily
- Current backends evolve consistently
- No technical debt from compromises
- Long-term maintainability guaranteed

---

*Document Version: 3.0 (Design-First Approach)*
*Last Updated: 2024-09-23*
*Status: Active Development - Following Design-First Philosophy*
