// src/codegen/features.rs - FeatureSet and Feature enum definitions
// 第一性原理：显式声明每个架构的能力，避免"静默 no-op"或虚假宣称支持

/// Feature flags for codegen capabilities
///
/// **Tier Classification:**
/// - **Tier-1**: Core/Kernel/Libiberty deep support, CI fully enabled
/// - **Tier-2**: Hello/cross smoke tests (may return E0xxx errors)
/// - **Disabled**: Removed from registry, has_backend=false
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    // Core computational features
    CoreArith,  // Basic arithmetic operations
    CoreCall,   // Function calls (syscall vs library)
    Float,      // Floating point support
    LongDouble, // 80/128-bit floating point
    Int128,     // __int128 support
    Bitfield,   // Bit field structs

    // Variable and memory management
    VaList,  // Variable arguments
    Tls,     // Thread-local storage
    Atomics, // Atomic operations

    // Code generation features
    InlineAsm(AsmLevel), // Inline assembly capability level
    Vector,              // SIMD/vector operations

    // Debugging and safety
    Sanitizer(SanKind), // Specific sanitizer type
    CfiUnwind,          // Cross-platform unwind info
    StackProtect,       // Stack protection cookies

    // Performance features
    ParallelCodegen, // Parallel code generation (currently x86-only)

    // Optimization features
    Lto,         // Link-time optimization
    BoundsCheck, // Array bounds checking
    Trapv,       // Trap on overflow

    // Output format
    ObjectFormat(ObjectFmt), // Elf / Coff (Coff only on x86+Windows)

    // Debugging info
    Dwarf(u8), // DWARF version (0 if not supported)

    // Architecture-specific
    RiscVBorrowedRegs, // RISC-V borrowed registers
}

/// Inline assembly level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AsmLevel {
    Minimal, // Basic inline asm
    Partial, // Limited constraints
    Full,    // Full GCC-style asm
}

/// Sanitizer kinds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SanKind {
    Address,
    UndefinedBehavior,
    Memory,
    Thread,
}

/// Object file formats
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectFmt {
    Elf,
    Coff,
}

impl Default for Feature {
    fn default() -> Self {
        Feature::CoreArith
    }
}

/// Feature set for a backend implementation
///
/// This represents what capabilities the backend actually supports.
/// It should match behavior probes from tests (see `check_feature_matrix` job).
#[derive(Debug, Clone, Default)]
pub struct FeatureSet {
    pub features: Vec<Feature>,
}

impl FeatureSet {
    /// Check if a specific feature is supported
    pub fn has(&self, feature: Feature) -> bool {
        self.features.contains(&feature)
    }

    /// Get list of all supported features
    pub fn all_features(&self) -> &[Feature] {
        &self.features
    }

    /// Create a feature set with core capabilities only
    pub fn tier1_core() -> Self {
        Self {
            features: vec![
                Feature::CoreArith,
                Feature::CoreCall,
                Feature::Float,
                Feature::Int128,
                Feature::VaList,
                Feature::Bitfield,
            ],
        }
    }

    /// Create a feature set with full support
    pub fn full() -> Self {
        Self {
            features: vec![
                Feature::CoreArith,
                Feature::CoreCall,
                Feature::Float,
                Feature::LongDouble,
                Feature::Int128,
                Feature::Bitfield,
                Feature::VaList,
                Feature::Tls,
                Feature::Atomics,
                Feature::InlineAsm(AsmLevel::Full),
                Feature::Vector,
                Feature::Sanitizer(SanKind::Address),
                Feature::Sanitizer(SanKind::UndefinedBehavior),
                Feature::CfiUnwind,
                Feature::StackProtect,
                Feature::ParallelCodegen,
                Feature::Lto,
                Feature::BoundsCheck,
                Feature::Trapv,
                Feature::ObjectFormat(ObjectFmt::Elf),
                Feature::ObjectFormat(ObjectFmt::Coff),
                Feature::Dwarf(4),
                // RiscVBorrowedRegs 不在 full() 里: 它是 RISC-V 专属特性，
                // 由 riscv64_cross() 单独声明; 放在 full() 会让 x86_64 谎报。
            ],
        }
    }

    /// Tier-1 交叉后端能力集 (aarch64)。
    ///
    /// 每一项都以 `scripts/probe_backend_features.sh` 的**产物指纹级**判据
    /// (artifact 级：节名 / UND 符号 / ELF 头) 为准；探针与这张表不一致时
    /// CI 直接变红，所以这里只列实测拿得出指纹的项。**不**由 `full()` 减法推导。
    ///
    /// 2026-09 实测下线的 4 项（命令与输出摘要）：
    ///   * `CoreCall`  ❌ —— `dcc --target aarch64-linux-gnu -c` 一个只声明
    ///     `int callee(int);` 的 TU，`readelf -sW` 里**没有** UND 的 `callee`，
    ///     反而 `-S` 输出里 `callee:` 被当成**本 TU 函数定义**发出来了（伪造空函数体）；
    ///     跨 TU 调用因此不可链接。→ 见矩阵文档「遗留事项」。
    ///   * `Sanitizer(Address)` / `Sanitizer(UndefinedBehavior)` ❌ ——
    ///     `-fsanitize=address` / `-fsanitize=undefined` 直接
    ///     `error[E0312]: unsupported IR expression on aarch64: ControlFlow`，
    ///     插桩需要的分支 lowering 不出来（x86_64 上同一 fixture 有 `__asan_load4`）。
    ///   * `StackProtect` ❌ —— `-fstack-protector-strong` 直接
    ///     `error[E0312]: unsupported IR expression on aarch64: Assign`，
    ///     cookie 赋值就写不下去（arm 上同一选项能留 `__stack_chk_fail` UND 引用）。
    ///
    /// 保留项的实测依据：
    ///   * `CfiUnwind` ✅ / `Dwarf(4)` ✅ / `ObjectFormat(Elf)` ✅ —— `.eh_frame`、
    ///     `.debug_info`+`.debug_line`+`.debug_abbrev`、`ET_REL` 均指纹命中。
    ///   * `Lto` ❌ —— `-flto -c` 产物里 `.dcc.lto`/`.llvm.lto` 命中 0。
    ///   * `Tls` ❌ —— `-c` 产物里没有 `.tdata`/`.tbss` 节。
    ///   * `Atomics`/`Vector`/`Int128`/`LongDouble`/`InlineAsm(Partial)` 只有
    ///     **accept 级**证据（接受构造且发出非空 `.text`），抓不到静默 no-op，
    ///     故保留声明但**不得当门禁依据**（见 `◐` 图例）。
    pub fn tier1_cross() -> Self {
        Self {
            features: vec![
                Feature::CoreArith,
                Feature::Float,
                Feature::LongDouble,
                Feature::Int128,
                Feature::Bitfield,
                Feature::VaList,
                Feature::Atomics,
                Feature::Vector,
                Feature::InlineAsm(AsmLevel::Partial),
                Feature::CfiUnwind,
                Feature::ObjectFormat(ObjectFmt::Elf),
                Feature::Dwarf(4),
            ],
        }
    }

    /// Tier-1 RISC-V 交叉后端能力集 (riscv64)。
    ///
    /// 这里**故意不再** `Self::tier1_cross()` 继承：riscv64 比 aarch64 还少一项
    /// 已声明能力，用继承就没法表达减法，会把谎报留在表里。
    ///   * `LongDouble` ❌ —— `dcc --target riscv64-linux-gnu -c` 一个 `long double`
    ///     取负的 fixture 直接 `error[E0314]: unsupported float IR expression on
    ///     riscv64: Unary(Neg)`（aarch64 同一 fixture 能过）。
    /// 其余项与 `tier1_cross()` 实测结果一致（`CoreCall`/`Sanitizer(*)`/`StackProtect`
    /// 同样为 ❌，理由同上）。`RiscVBorrowedRegs` 仅在 RISC-V 上名副其实。
    pub fn riscv64_cross() -> Self {
        Self {
            features: vec![
                Feature::CoreArith,
                Feature::Float,
                Feature::Int128,
                Feature::Bitfield,
                Feature::VaList,
                Feature::Atomics,
                Feature::Vector,
                Feature::InlineAsm(AsmLevel::Partial),
                Feature::CfiUnwind,
                Feature::ObjectFormat(ObjectFmt::Elf),
                Feature::Dwarf(4),
                Feature::RiscVBorrowedRegs,
            ],
        }
    }

    /// Tier-2 交叉后端能力集 (arm / mips / mips64)。
    ///
    /// 这张表是三个后端的**交集**（一个 ctor 服务多个架构，只能取下界），
    /// 所以 arm 实测做到的项（如 `-fstack-protector-strong` 留下
    /// `__stack_chk_fail` UND 引用）若 mips 做不到，这里就必须是空。
    ///
    /// 2026-09 实测：
    ///   * `CoreCall` ❌ —— arm 把 `callee` 记成 `Type=FILE` 的 UND 符号（链接时
    ///     不会被当成调用目标），mips 干脆没有该 UND 条目（extern 声明被物化成
    ///     本 TU 定义）。→ 从本集合删除。
    ///   * `StackProtect` / `CfiUnwind` / `Dwarf(4)` / `ObjectFormat(Elf)` ✅ ——
    ///     arm、mips、mips64 均可指纹命中。mips64 曾因 ELF64 容器端序自相矛盾
    ///     （`e_ident` 写 MSB、头字段写 LSB，宿主 `objdump -h` 直接拒绝，见
    ///     `non_x86_c_mode_produces_valid_elf`）而整列 `?`（UNK）；该缺陷已修
    ///     （`elf.rs::build_elf_rel_generic` 现按文件自己声明的 `EI_DATA` 落地），
    ///     所以本集合不再依赖任何不可判定的格子。
    ///     但要知道一条边界：`StackProtect` 的判据靠 `readelf -sW` 读 UND 符号名，
    ///     而 ELF64 侧另有「首条空符号 25 字节 ⇒ symtab 整体错位」的缺陷（MSB 路径已修，
    ///     aarch64/riscv64 仍在）——那会让格子变成**测不出的假阴性**，不是“不支持”，
    ///     不要据此把 `StackProtect` 从本集合删除。
    ///   * `Lto` / `Tls` / `Trapv` / `BoundsCheck` / `Vector` / `Atomics` /
    ///     `Int128` / `LongDouble` ❌ 或未验证 —— 不声明。
    pub fn tier2_cross() -> Self {
        Self {
            features: vec![
                Feature::CoreArith,
                Feature::Float,
                Feature::Bitfield,
                Feature::VaList,
                Feature::InlineAsm(AsmLevel::Minimal),
                Feature::CfiUnwind,
                Feature::StackProtect,
                Feature::ObjectFormat(ObjectFmt::Elf),
                Feature::Dwarf(4),
            ],
        }
    }
}

/// Behavior probe result codes (for CI verification)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeResult {
    Ok,             // Feature works as expected
    Err(String),    // Feature returns error code
    MissingFixture, // No test fixture available
    Unsupported,    // Architecture doesn't support this feature
}
