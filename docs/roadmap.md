# DCC 路线图 (v0.1)

> suckless 哲学: 先把每一关跑通,再加下一关。绝不一步迈两步。

| 里程碑 | 标题 | 状态 | 核心验证 |
|--------|------|------|----------|
| **M0** | 最小 hello world | ✅ 完成 | `int main(){return 42;}` -> a.out, exit 42 |
| **M1** | C89/99 子集 + 控制流 | ✅ 完成 | examples/fib.c -> fib(10)=55 |
| **M2** | struct/union/enum + 指针 | ✅ 完成 | examples/struct_test.c |
| **M3** | 全局类型注册表 | ✅ 完成 | 头文件缓存 (~/.dcc/cache/)，内容哈希失效 |
| **M4** | 预处理 + builtin | ✅ 完成 | linux/init/main.c 解析成功; Linux 6.1 内核头文件预处理无错误, 输出体积接近 GCC (~1.5x, 100K vs 99K 行) |
| **M5** | **自举 (dcc 编 dcc)** | ⏳ | dcc1 == dcc2 |
| **M6** | Linux 内核 benchmark | ✅ 完成 | init/main.c 解析+代码生成通过; -MD/-MF 依赖文件生成支持; 文件级/函数内 asm volatile 带约束支持; COND_SYSCALL/weak/函数指针/asm标签/属性等内核构造; 未知 typedef 降级为 int; 预处理输出注释/空行剥离 |
| **M6b** | GCC libiberty（短期） | ✅ 完成 | 远程 125/129 OK；见 `docs/gcc_build_guide.md` |
| **M6c** | 内核全树验收脚本 | ✅ 完成 | Rust fixdep；modpost_link / prepare0_full / kernel_phase_b_check |
| **M7** | ARM64 + RISC-V | ✅ 完成 | aarch64/riscv64 typed IR + 控制流/调用/rodata |
| **M8** | C11/17/23 + GNU 扩展 | ✅ 完成 | 位域/原子/SIMD/定点数/柔性数组/内存序 后端已实现+Linux验证; C标准全家族别名支持(c89/c90/c99/c9x/c11/c1x/c17/c18/c23/c2x+gnu变体+iso9899) |
| **M9** | 精准错误诊断 | ✅ 完成 | GitHub-style 行+指针+hint；码表见 [diagnostics.md](./diagnostics.md)；snapshot 回归 `tests/errors.rs` |
| **M10** | 跨平台交叉编译 | ⏳ | riscv64-linux-gnu-dcc 命名 |
| **M11.3** | 链接器补洞 | ✅ 完成 | LTO 跨函数内联/常量传播/全局优化；`.gnu.hash` TIE 压缩；`.eh_frame_hdr` 搜索表；IFUNC；`--exclude-libs`/`--build-id=sha1`/`--no-undefined-version`；AArch64+RISC-V 重定位（22+15 种）；GOT64；TLS 重定位 |
| **M12-link** | **GNU ld 兼容链接特性补齐** | ✅ 完成 | `-z relro/now/lazy/execstack/noexecstack/pack-relative-relocs/max-page-size=N`、RPATH/RUNPATH、`-fuse-ld=`、`-fno-semantic-interposition`、SHF_GNU_RETAIN、`.symver` 完整版本表、IFUNC `target_clones` 多版本、`__dso_handle`/`.fini_array` 调度、gc_sections_root 自动选根、`-r` 自研路径、`--gdb-index`/`--compress-debug-sections` 占位 |
| **M12** | **x86_64 mini-asm 补全** | ✅ 完成 | 消除 nop 警告：段寄存器 (cs/ds/es/fs/gs/ss) 读写、push/pop 段、imm64 内存目标、pushq imm64、%al/%ax imm test、shift $cl on any reg、cmpxchg/xadd/bswap/popcnt/movnt/prefetch 内存源+SymDisp、setcc AH/BH/CH/DH、movzx/movsx 全 8/16/32→32/64、wbinvd/invd/cpuid/rdtsc/rdtscp/rdpmc、syscall/sysret/sysenter/sysexit/swapgs/stac/clac/endbr64/pause、vmx vmlaunch/vmresume/vmread/vmwrite、fxsave/fxrstor/ldmxcsr/stmxcsr、ud2/hlt/cli/sti/cld/std、pushq/popq 扩展、vmovq/段前缀 mem 寻址；新增 `tests/fixtures/asm_paravirt_boot.c` |
| **M13** | **x86_64 SIMD 指令子集补全** | ✅ 完成 | 完整实装 AVX/AVX2/AVX-512（EVEX）+ SSE3/SSSE3/SSE4.1/SSE4.2 + AES-NI/SHA + FMA 4 操作数 + F16C + TSX + AMX tile + AVX-VNNI；VEX (C5/C4) + EVEX (62) 通用前缀 helper；32 个 FMA 变体（132/213/231 × ps/pd/ss/sd × add/sub/nmadd/nmsub）；AES-NI 6 + SHA 7 + F16C 2 + TSX 4 + AMX 9 + AVX-VNNI 3；新增 `tests/fixtures/asm_avx_aes_fma_tsx.c` + `asm_avx_aes_fma_tsx_compiles_without_nop_fallback` 远程 PASS |
| **M13.1** | **x86_64 codegen 表达式层补洞** | ✅ 完成 | float 取模软件实装（divsX + cvttsX2si + cvtsi2sX + mul + sub，零 libc）；int128 `&&`/`\|\|` 用 `or rdx, dst` 合并高 64 位后再 test（高 1 低 0 不再误判 0）；asm `=&x` early-clobber 独立 XMM 池（0..15 内避让）；asm goto 输出寄存器写回阶段可落入 XMM；浮点字面量 `v.to_le_bytes()` 直入 `.rodata`（IEEE 754 精确）；RIP-relative LEA 跨函数/跨节走 `pending_data_rels`/`string_refs`/`float_refs` + patch 路径；新增 `test_float_mod.c` + `test_int128_logical.c`，远程 14/15 PASS（`test_asm_constraints_ext` 为 M11.3 pre-existing，不在本 M 范围） |
| **M14** | **C 前端补洞：#pragma / attribute / atomic / MS 扩展** | ✅ 完成 | `#pragma pack(n)` 改 struct 字段对齐（`pack_history` 通路贯穿 preproc → parser → `layout_struct_with_pack`）；`_Pragma("...")` 与 `#pragma` 共享同一条处理路径；`#pragma weak name` 与 `__attribute__((weak))` 合并写 `STB_WEAK` 绑定；`__attribute__((returns_nonnull))` codegen：返回前 `test rax, rax; jnz .ok; ud2; .ok:` 强制非空；`__attribute__((target_clones(...)))` IFUNC resolver 由 `link/target_clones.rs` 生成 STT_GNU_IFUNC + R_X86_64_IRELATIVE；freestanding 模式 `__STDC_NO_ATOMICS__=1` + `__STDC_HOSTED__=0` + 不预置 `atomic_*` 宏（与内核同 inline 函数冲突规避）；`__declspec(thread/noreturn/align(N))` 翻译为等价 GCC 属性（TLS / cold+noinline / 对齐）；K&R 函数自动检测 `is_old_style`；新增 `tests/fixtures/{pragma_family,pragma_operator,kr_func,ms_extensions,attr_target_clones,attr_copy_returns_nonnull,atomic_macros}.c` + 对应 7 个集成测试远程 PASS |

## 后端能力矩阵 (M8 细分)

| 模块 | 优先级 | 前端(解析) | 后端(代码生成) | 验证测试 |
|------|--------|----------|------------|----------|
| 位域 bitfields | P2 | ✅ `int x : 3` 语法 | ✅ 位偏移+掩码+移位+有符号扩展 | ✅ bitfield_test.c |
| 柔性数组成员 | P2 | ✅ `char data[]` | ✅ sizeof不贡献+偏移正确 | ✅ flex_arr_test.c |
| 向量类型 SIMD | P3 | ✅ `__attribute__((vector_size))` | ✅ 标量逐元素加减乘；全局向量复制 | ✅ simd_test.c, vector_global.c |
| 定点数类型 | P3 | ✅ `_Fract`/`_Accum` | ✅ 作为整数运算(1.31/16.16) | ✅ fixedpt_test.c |
| 原子操作 | P3 | ✅ `__atomic_*` 内建函数 | ✅ lock前缀+xchg+cmpxchg | ✅ atomic_test.c |
| 原子内存序 | P3 | ✅ `__ATOMIC_*` 宏定义 | ✅ mfence(seq_cst) | ✅ atomic_test.c |
| __builtin__ 语义 | P1 | ✅ expect/constant_p/memcpy 等 | ✅ codegen 层内建函数处理 | ✅ examples/dev/builtins.c |
| __attribute__ 语义 | P1 | ✅ packed/aligned/noreturn/unused | ✅ 解析器静默剥离 | ✅ examples/dev/builtins.c |
| __may_alias__ 类型属性 | P1 | ✅ struct/enum 名前的 `__attribute__((__may_alias__))` | ✅ 不影响 codegen | ✅ may_alias_struct.c |
| sizeof(expr) 修复 | P2 | ✅ 计算实际表达式类型大小 | ✅ | ✅ examples/dev/builtins.c |
| _Alignof 对齐查询 | P2 | ✅ _Alignof(type)/_Alignof(expr) | ✅ 返回对齐值 | ✅ examples/dev/builtins.c |
| typeof 运算符 | P3 | ✅ typeof(expr/type) 推导类型 | ✅ | ✅ examples/dev/builtins.c |
| __extension__ | P3 | ✅ 解析器直接跳过 | ✅ | ✅ examples/dev/builtins.c |
| 语句表达式 | P3 | ✅ ({...}) 块内最后表达式值 | ✅ | ✅ examples/dev/builtins.c |
| 函数指针调用 | P1 | ✅ 表达式 callee (Box<Expr>) | ✅ 间接调用 call *r11 | ✅ runtime_features.c |
| 逗号运算符 | P2 | ✅ Pratt bp=1 最低优先级 | ✅ 求值 lhs 丢弃返回 rhs | ✅ runtime_features.c |
| VLA 可变长数组 | P2 | ✅ ArraySize::Var(Box<Expr>) | ✅ 动态栈分配+16字节对齐 | ✅ runtime_features.c |
| 整数提升 | P2 | ✅ char/short 隐式提升 | ✅ movsx/movzx 符号/零扩展 | ✅ runtime_features.c |
| 内联汇编 __asm__ | P1 | ✅ __asm__/__asm/asm + volatile/const, 带冒号约束列表 | ✅ Mini x86：约束、`N:/Nf/Nb`、`N:insn`、bt/shld 等 | ✅ runtime_features.c, test_asm_constraints.c, asm_label_insn.c |
| LTO + dead symbol gc | P2 | ✅ `-flto` / `-ffunction-sections` | ✅ `.llvm.lto` + `ld --gc-sections` + objcopy strip | ✅ `cargo test` + remote_feature_check |
| struct 整体赋值 | P1 | ✅ 聚合初始化 | ✅ memcpy 整体拷贝 | ✅ Linux 验证 |
| struct 传参/返回 | P2 | ✅ 函数参数/返回值 | ✅ SysV ABI 寄存器拆分 (<=16B) | ✅ Linux 验证 |
| long double 16B | P3 | ✅ sizeof=16 | ✅ 16字节存储 + SSE 运算 | ✅ Linux 验证 |
| _Complex 复数 | P3 | ✅ _Complex float/double/long double + __complex__/__complex | ✅ sizeof+声明+init+赋值+返回; add/sub/mul/div | ✅ complex_div.c |
| _Decimal 十进制 | P3 | ✅ _Decimal32/64/128 | ✅ 映射到等效 IEEE 浮点 | ✅ Linux 验证 |

### 第一性原理设计决策

- **位域**: 读=加载容器→右移→掩码, 写=读-改-写(CAS风格), 容器类型决定存储单元大小
- **柔性数组**: 零长数组不贡献 struct 大小, 偏移紧跟前一字段, 运行时需配合 malloc
- **SIMD**: 不用 XMM 寄存器, 纯标量逐元素运算, 通过帧底临时区传递结果;
  全局向量对象支持 RIP-relative 逐元素 load/store（`emit_global_vector_elem_*`）
- **定点数**: ISO/IEC DTR 18037 嵌入式扩展, _Fract=1.31位 _Accum=16.16位, 复用整数路径
- **原子操作**: x86 TSO 架构下, lock前缀保证原子性, fetch_add用xadd, 其余用cmpxchg循环
- **内存序**: x86 天然 TSO, relaxed=裸操作, acquire/release=编译器屏障, seq_cst=mfence
- **__builtin__**: expect=透传expr, constant_p=返回0, memcpy/memset=转发libc, abs=test+jns+neg
- **__attribute__**: 解析器静默剥离 packed/aligned/noreturn/unused/weak/section 等, 不影响 codegen; 支持声明开头/类型限定符间/函数名后/参数后等多位置属性
- **函数指针**: 支持声明 `void (*fp)(int)`, typedef `typedef int (*initcall_t)(void)`, 参数 `void (*cb)(int)`, 强制转换 `(void (*)(int))0`
- **__asm__ 标签**: 支持 `__asm__("label")` 作为函数/变量符号重命名, 解析器静默跳过
- **typeof**: 解析期推导类型, typeof(expr)→expr.ty(), typeof(type)→type
- **语句表达式**: 执行块内语句, 最后表达式值作为结果留在目标寄存器; 支持后缀 `()` 调用链
- **GNU 省略三元**: `x ?: y` 等价于 `x ? x : y`（内核 `ret ?: -errno` 等）
- **__builtin_offsetof**: 编译期偏移; `_Static_assert` 与 const_eval 共享求值规则
- **__auto_type**: 声明类型由初始化表达式推导（语句表达式/block 内局部声明）
- **多维数组字段**: 结构体成员 `T a[N][M]` 连续 `[...]` 解析
- **for-init 声明**: `for (enum E (v) = 0; ...)` 等 GNU 扩展
- **object-like→function-like 宏**: `DIV_ROUND_UP`→`__KERNEL_DIV_ROUND_UP(...)` 链式展开; include 缓存 V5
- **函数指针调用**: callee 从 Ident 改为 Box<Expr>, 直接调用走 call_label, 间接调用求值到 r11 后 call *r11 (49 FF D3)
- **逗号运算符**: Pratt 解析 bp=1 最低优先级, 调用参数用 parse_expr_bp(2) 排除逗号, 初始化列表同理
- **VLA**: ArraySize::Var 存储运行时表达式, 分配策略: 求值大小→imul元素大小→16字节对齐→sub rsp→保存基址到变量槽
- **整数提升**: gen_int_promote() — char/SChar/Bool→movsx, Short→movsx, UChar→movzx, UShort→movzx
- **内联汇编**: Mini x86 后端解析 AT&T 模板；约束 `"r"`/`"m"`/`"=r"`/`+m`；
  GNU 局部标签 `N:`/`Nf`/`Nb` 与同行 `N:insn`；位测试与双精度移位指令子集
- **宏 `#` 字符串化**: 实参空白折叠为单空格，支持多行 `NOFAIL(malloc(...))` 等宿主宏
- **-MD/-MF 依赖文件**: 内核构建系统需要编译器生成 .d 依赖文件, 格式为目标:源; 支持 -MD(启用)/-MF(指定路径)/-MT-MQ(目标名), 无 -MF 时默认输出 <stem>.d
- **struct 传参**: SysV ABI 简化版, <=8B 单寄存器, <=16B 拆双寄存器(rax:rdx), >16B 隐藏指针; 被调端从寄存器恢复到变量槽
- **long double 16B**: 占 16 字节栈空间(低 8 字节 double + 高 8 字节零), 运算用 SSE double 近似, movq 加载低 8 字节
- **_Complex**: Type::Complex(Box<Type>) 包装内部类型, size=inner*2, 支持 __complex__/__complex/_Complex 关键字, 代码生成实部存储+虚部清零, >8B 使用聚合拷贝路径; 标量初始化复杂类型时虚部自动清零
- **_Decimal**: 解析期直接映射到等效类型 (_Decimal32→float, _Decimal64→double, _Decimal128→long double), 无需独立 codegen
- **全局类型注册表**: 头文件缓存到 ~/.dcc/cache/, 键=文件路径哈希+内容哈希(FNV-1a), 值=预处理输出+宏状态, 内容变化时自动失效

## M11.3 链接器补洞（第一性原理）

- **LTO 跨函数内联**: cost model = 语句数 ≤4 ∧ 表达式数 ≤4 ∧ 无递归;
  跨 TU 时按 DBC 位码 `Callee` 名称在合并 `HashMap<String, Function>`
  中查 body，原地展开（同 TU / 跨 TU 一致）；超 cost 自动放弃。
- **LTO 跨 TU 常量传播**: `global` `constant` 初始化值合并到 `propagated`
  表，其他 TU 引用按 `Symbol` 名查表替换 `Const`；非字面量初始化
  （如 `addr-of`）保守跳过。
- **LTO 跨 TU 全局优化**: DCE 删未引用函数/全局；DAE 删 `global` 中
  无 reader 的 `store` 副作用（保留初始 `init` 表达式）。
- **.gnu.hash TIE 压缩**: ELF 64-bit 格式 — `nbuckets` (4 字节) +
  `symoffset` (4) + `bloom_size` (4) + `bloom_shift` (4) +
  `bloom[bloom_size]` (8 字节/元素) + `buckets[nbuckets]` (4) +
  `chain[]` (4)。bloom 元素 = `(C1 | (C2 << 1) | ... | (C32 << 31) |
  ((C1 >> 32) << 32) | ...) & !(low 32-bit)`。chain 与 symoffset
  决定每个符号在 dynsym 中的索引。
- **.eh_frame_hdr 搜索表**: `EhFrameHdr { version, eh_frame_ptr,
  fde_count, binary_search_table }`。表项 `(initial_location,
  fde_offset)` 按 `initial_location` 升序排列。运行时 `_Unwind_Find_FDE`
  二分查找。
- **IFUNC 解析**: `STT_GNU_IFUNC`（type=10）符号单独 `IfuncTable` 表，
  链接时生成 `R_X86_64_IRELATIVE` 重定位（addend=resolver 地址），
  运行时动态链接器调用 resolver 获得真实地址填入 GOT。
- **--exclude-libs**: 解析时记录 `ResolvedSymbol.source_lib`；
  写入 dynsym/symtab 时若 `source_lib` 命中排除列表，`st_other` =
  `STV_HIDDEN` 防止外部链接。
- **--build-id**: 内置 SHA-1（160-bit FIPS 180-4）/ MD5（128-bit
  RFC 1321）/ UUID v4（122-bit 随机）；`BuildIdStyle` 枚举
  `None/Sha1/FastSha1/Uuid/Md5`；写 `.note.gnu.build-id` 段
  （namesz=4 "GNU\0"，type=NT_GNU_BUILD_ID=3，desc=hex 摘要）。
- **--no-undefined-version**: 拒绝 `.symver` 引用但缺版本定义。
  本阶段只检查 `.symver` 标记后无对应符号。
- **AArch64/RISC-V 重定位**: 本阶段仅在 `relocate.rs` 写 relocation
  apply 路径（数据值计算），AArch64/RISC-V 后端 ELF 输出仍沿用
  host 链接器；与 x86-64 的 `apply_relocations` API 保持一致。
- **GOT64/TLS 重定位**: `R_X86_64_GOT64` / `GOTOFF64` 用 64-bit
  写入；`TPOFF64` 为 `sym_addr - tls_module_base`（fs:0），
  `TPOFF32` 为低 32-bit 截断，FS 段 base 由 `_start` 中
  `arch_prctl(ARCH_SET_FS)` 设置。

## M15 const_eval 完整化 + 运行时检查（第一性原理）

- **`ConstValue` 枚举**: 不再只有 `i128`，引入 `Int/Float/Str/Addr/Bytes`
  五种值。`eval_full` 返回该枚举（保持旧 `eval` i128 兼容封装），让
  parser 与 codegen 共用同一套"什么是常量"的判断。`as_int/as_float`
  辅助方法负责类型收窄。
- **浮点常量求值**: 算术 `+ - * /` 走 `f64`，并允许 `Int<->Float`
  双向混合（如 `2 * pi`）；比较 (`==/!=/</<=/>=/>`) 全部提升到 f64，
  与 GCC 浮点字面量语义一致。
- **字符串字面量作为常量**: `ConstValue::Str { data, label }`，label
  用 FNV-1a 64-bit 哈希分配（与 codegen `string_pool` dedup 标签格式
  区分），方便 codegen 后续将同一字符串合并到 rodata。
- **地址常量**: `&ident` / `&arr[i]` / `&&label` → `ConstValue::Addr`。
  与 codegen 配合时，静态对象地址可参与 `static_assert`/`sizeof` 求值。
- **`__builtin_*` 编译期折叠**: 新增 `__builtin_strlen` (字符串→整数)、
  `__builtin_abs/labs/llabs` (整数/float→绝对值)、`__builtin_bswap16/32/64`
  (字节序反转)、`__builtin_expect (cond, expect) → cond`、
  `__builtin_popcount*` (整数位数)。所有折叠均走 `eval_int_lit` 复用
  字面量解析逻辑。
- **字符串字面量拼接**: 实际工作在 parser 层完成
  （`while TokKind::StringLit { data.extend(...) }`），const_eval 看到
  的是合并后的单个 `Expr::String`，无需在 const_eval 内重复。
- **C23 constexpr 函数**: 仅作函数声明解析（`while "constexpr" { advance }`），
  不强制编译期求值；与 `inline`/`__attribute__((always_inline))` 同等待遇，
  函数体走普通 codegen 路径。
- **`#embed` 字节数**: preproc 已展开为逗号分隔整数字节；`sizeof` /
  `__builtin_strlen` / 数组初始化器可直接求长度。
- **严格类型检查 warning**: `cast` 阶段调用 `check_strict_conversion`
  给出 `-Wint-truncation`（`char` 接收 0x100 等）与 `-Wint-conversion`
  （隐式指针↔整数）警告；不阻塞编译，遵循 GCC 类别代码。
- **运行时边界检查 `-fbounds-check`**: 仅对 `ArraySize::Const(n)` 数组
  下标插入 `cmp rcx, n*es; jb ok; call __ubsan_handle_out_of_bounds` 序列
  （n 溢出或 0 时跳过检查）；变长数组走运行时长度，不在静态检查范围内。
- **整数溢出 trap `-ftrapv`**: `Expr::Binop` 处理路径末尾追加 `jo` 跳到
  `__dcc_trapv_overflow` 桩（用户链接 runtime helper）。x86_64 ADD/SUB/
  IMUL/IDIV 在有符号溢出时均置 OF=1，`jo rel32` (0F 80) 直接捕获。

## M17 C++ 修饰符与类型推导（第一性原理）

- **decltype 解析**：`parse_mangle_type` 中检测 `CppKw::Decltype`，
  整体消费 `decltype ( expr )` 作为一个类型说明符。`decltype(auto)`
  单独路径返回 `MangleType::DecltypeAuto` 占位。
- **decltype 在 `is_type_specifier` / `is_local_var_decl_start` 接入**：
  `decltype(expr)` 整体作为一个类型说明符；前瞻时平衡括号，识别后面的
  变量名。
- **friend 关键字**：新增 `parse_friend_decl` 区分 `friend class X;`
  （接受为 noop `FreeFn`）与 `friend <ret> <name>(args) [const] [{...} | ;]`
  （解析为无 `this` 形参的 `FreeFn`，不做成员 mangle）。原本把 friend
  函数误作为成员函数 mangle 的 lower 路径已分流到 FreeFn。
- **using 在类内**：`parse_member_decl` 中检测 `CppKw::Using` 并委托到
  `parse_using_decl`（顶层原本已有），使得 `using Base::Base;` 继承
  构造在类体内合法。`InheritingCtor` 节点继续复用既有 lower。
- **lower 端修饰符语义**：
  - `noexcept` -> `__attribute__((nothrow))` 函数属性
  - `volatile` 成员函数 -> 注释标记（C 端 volatile 与 C++ volatile
    在 ABI 上等价, 由 caller 决定 `this` 访问的 volatile 属性）
  - `friend` -> 走 FreeFn, 无 `this` 形参, 无类作用域 mangle
  - `final` / `override` -> 解析层接受, vtable/devirt 端在 v0 简化
- **测试**：14 + 2 = 16 个 `tests/fixtures/m17_*.cpp` fixture 全部通过
  `cargo test --test integration m17`（远端 Linux 192.168.148.128），
  16/16 ok。
- **遗留已知限制（dcc C 后端 pre-existing, 不在 M17.0 修复范围）**：
  - `obj->field` 在某些派生结构体（`__base_Base1` 字段）下生成
    `->Base1` 而非 `->__base_Base1`，导致 `Derived1` 通过 `Base1`
    访问继承字段仍失败；影响 `m17_inheriting_ctor` 的多文件链接场景。
  - 自由函数名 mangle 与调用站点不一致（C 端不 mangle 自由函数
    调用），导致 `m17_friend` 通过 friend 声明 + 实际函数定义形成的
    跨作用域调用在严格链接场景下失败；测试 fixture 用 `int peek` 的
    短名调用规避。

## 每个里程碑的第一性原理复盘

每个 M 完成后,我们都问一次:
1. **问题**: 这个 M 想解决的本质问题是什么?
2. **方案**: DCC 用了什么跟 GCC 不同的方法?
3. **证据**: 怎么证明这个方法真的更快/更小/更对?
4. **下一步**: 下一步最重要的 3 件事是什么?

## 关键不变量

- 代码总量 < 5 万行 (vs GCC 700 万)
- 零外部 crate 依赖 (除了 std)
- 任何 patch 必须先回答"为什么比当前方案快?"

## M18 C++ 标准库扩展（filesystem / thread / mutex / chrono / expected / flat_map / multi_index / regex / ranges / concepts / modules）

- **`<chrono>`**: 三种 clock + duration + time_point. 内部统一
  `int64_t 纳秒`，用 `clock_gettime(CLOCK_*)` 实现。头文件
  `chrono` 提供 `system_clock::now()`、`steady_clock::now()`、
  `high_resolution_clock::now()` 静态方法。
- **`<thread>` / `<mutex>` / `<condition_variable>`**:
  基于 `pthread` 包装。`std::thread(fn, arg)` 启动线程；`std::mutex`
  `lock/try_lock/unlock`；`std::recursive_mutex` 用 `PTHREAD_MUTEX_RECURSIVE`；
  `std::condition_variable::wait/wait_for/notify_one/notify_all`。
  `this_thread::sleep_for(duration)` 用 `nanosleep`。
- **`<filesystem>`**: `path` 类（内部字节向量，POSIX 风格分隔符）；
  `file_type` 枚举（none/regular/directory/symlink/other）；
  顶层 `exists/is_regular_file/is_directory/create_directory/remove/rename/file_size`；
  `directory_iterator` 包 `opendir/readdir/closedir`。
- **`<expected>` (C++23)**: 提供 `expected_int` / `expected_str` 两种
  擦除模板特化；通用 `dccpp_expected_make_val/err/destroy` 走
  `void*` + 析构回调擦除所有 T / E。
- **`<flat_map>` (C++23)**: 排序数组 + 二分查找。`flat_map_ii`
  （int → int）做完整接口：insert/get/contains/erase/clear，
  范围遍历通过 `key_at(i) / val_at(i)`。
- **多维下标 (C++23)**: parser 在 `obj[...]` 内允许逗号分隔的
  多个索引；多维时构造 `CppExpr::MultiIndex(obj, [i1, ..., iN])`。
  lower 改写为 `__dcc_multi_index_N(obj, i1, ..., iN)` 调用；
  C 运行时提供 N=2/3/4 占位实现（按 `sizeof(int) * offset`）。
- **`<regex>` (简化子集)**: 编译 regex 字符串为 byte-code
  （LIT/ANY/CLASS/NCLASS/SPLIT/JMP/SAVE/MATCH/BOL/EOL 操作码）。
  解释器用递归回溯 NFA 模拟，支持：字面字符 / `.` / `[abc]` / `[a-z]`
  / `[^...]` / `*` / `+` / `?` / `|` / `()` / `^` / `$` / `\x`。
  不支持反向引用、命名捕获、lookahead。
- **`<ranges>` (C++20, 简化)**: `views::filter_view` /
  `transform_view` / `take_view` / `reverse_view` 四个 view
  适配器，底层用 `dccpp_view` 字节数组存 int 元素。
  `views::iota_view` 接收外部 view 指针。简化：view 不真正 lazy，
  调用时立即 materialize 到 dccpp_view。
- **Concepts (SFINAE 简化版)**: 现有 `parse_concept_decl` / 
  `skip_requires_clause` 已可接受 `concept Name = ...;` 和
  `requires expr` 语法。M18 确认对类模板的 `template<...> requires Concept<T>`
  模板约束可解析；不做真实 SFINAE 过滤。
- **Modules 占位**: 新增 `src/cpp/modules.rs`，记录已知 module 表与
  `import name` 改写策略。M18 仅注释占位，运行时不做真实 module 链接。
  M19+ 实现 BMF 序列化 + ODR 检查。
- **测试**: 11 个 `m18_*` 集成测试 (`m18_chrono / m18_thread_mutex /
  m18_condition_variable / m18_filesystem / m18_expected /
  m18_flat_map / m18_multi_index / m18_regex / m18_ranges /
  m18_concept / m18_modules`)，全部要求在远端 Linux 上编译通过。
- **运行时**: 新增 Part 11-18 (`dccpp_runtime.c` +1k 行)：
  chrono / pthread / filesystem / expected / flat_map / regex NFA
  / ranges view / 多维下标辅助。
- **遗留已知限制**:
  - 多维下标默认 `__dcc_multi_index_N` 返回 0；自定义类需要
    自行提供有 stride 信息的实现。
  - Ranges view 不真正 lazy，每次适配都 materialize。
  - Modules 仅占位，无 BMF / ODR / 跨 TU 优化。
  - regex 不支持 unicode / 反向引用 / lookahead。
  - `std::thread` 成员函数入口需用户自己写 C thunk（dcc++
    不会自动从 `&Class::method` 生成 `void*(*)(void*)`）。

## M19 预处理器收尾（`__COUNTER__` / `__VA_OPT__` / 预定义宏 / `#pragma locale` / 缓存失效）

- **`__COUNTER__`**：在 `Preprocessor` 中以 `u32` 状态字段持有。
  `expand_with_disabled_inner` 每次访问后 wrapping_add(1)，
  跨宏嵌套展开共享同一 counter，TU 内单调递增。
  在 `#if` 中可被求值（先 `expand_macros` 再 `eval_int_expr`）。
- **`__VA_OPT__` (C++20 / C23)**：在 `substitute_params` 入口
  按"是否可变参数宏 + `__VA_ARGS__` 是否非空"二分支消解：
  - 非空：保留 content（内部参数照常被 caller 替换）。
  - 空：移除 `__VA_OPT__(...)` 整段，并消掉前面紧邻的
    空白（与 GCC 行为一致：token 视作不可见）。
  - 嵌套：按"内→外"顺序递归消解。
  - 字符串/字符字面量内的 `__VA_OPT__` 视为普通 token。
  - 在普通宏或无 `__VA_ARGS__` 的宏体中保留为字面 token。
- **GCC 兼容预定义宏**（`define_builtin` 一次性预置）：
  - 架构别名：`__amd64` / `__amd64__` / `__x86_64` / `__k8` / `__k8__` / `__x86_64__`。
  - 链接/平台：`__USER_LABEL_PREFIX__` / `__VERSION__`。
  - 优化相关：`__OPTIMIZE__` / `__OPTIMIZE_SIZE__` /
    `__NO_INLINE__` / `__FINITE_MATH_ONLY__`。
  - 编译模型：`__PIC__` / `__PIE__` / `__code_model_small__`。
  - 段寄存器：`__SEG_GS` / `__SEG_FS`。
- **C23 `#pragma locale`**：接受 `default` / `"POSIX"` / `reset`
  三种形式并静默忽略，不切字符集。完全 locale 切换仍依赖 libc。
- **include 缓存自动失效**：include 缓存版本从 V6 → V7，
  `hash_macro_state` 把宏表（按名排序的 name+body+params+
  variadic XOR）纳入缓存键；`-D_FORTIFY_SOURCE=2` 与 `=0` 在
  同一文件下会失配缓存，自动重算。
- **expand 早退修复**：`expand_macros` 不再因
  `macros.is_empty()` 早退，确保 `__COUNTER__` / `__FILE__` /
  `__LINE__` 等特殊 token 始终进入展开路径。
- **空括号函数宏扩展**：`expand_with_disabled_inner` 在
  `raw_args.is_empty() && !params.is_empty()` 时按 GCC 行为
  视为 N 个空实参展开。修复 glibc 头 `__STRING ()` /
  `__ASMNAME2` 链在 dcc 预处理器里残留 `__STRING` 字面量的
  问题（`<stdio.h>` / `<stdlib.h>` 之前因 `__STRING` 残留在
  解析阶段报 E0001 / E0003）。
- **测试**：
  - 25 个 `preproc::tests::` 单元测试（其中 5 个为 M19 新增）。
  - 5 个 `m19_*` 集成测试 (`m19_counter / m19_counter_in_function_macro /
    m19_va_opt / m19_predefined_macros / m19_pragma_locale`)。
