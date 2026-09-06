# DCC 编译器架构文档

## 1. 项目概述

DCC 是一个用 Rust 编写的 C11 编译器,支持 GNU 扩展,目标平台包括 x86_64、AArch64、ARM、RISC-V、MIPS 等。项目采用单遍编译架构,直接从 AST 生成机器码,不经过传统的中间表示(IR)阶段。

**核心特性:**
- 支持 C11 标准 + GNU 扩展(嵌套函数、复合字面量、语句表达式等)
- 多后端架构(x86_64 为主,其他架构正在开发中)
- 直接生成 ELF 可执行文件或可重定位目标文件(.o)
- 支持 SSA 优化 pass、LTO、增量编译
- 支持 Linux 内核特性(段寄存器、per-cpu 变量等)

**代码规模:**
- 主后端 `src/codegen/x86_64.rs`: **33,512 行**
- 解析器 `src/parser.rs`: **11,500 行**
- AST 定义 `src/ast.rs`: **1,518 行**

## 2. 编译流程

```
源代码 → 预处理(preproc.rs) → 解析(parser.rs) → AST → 代码生成(codegen/x86_64.rs) → 机器码
                                                      ↓
                                                 常量折叠(const_eval.rs)
                                                      ↓
                                                 SSA 优化(ssa.rs, ssa_passes.rs)
                                                      ↓
                                                 LTO(lto.rs)
```

**关键阶段:**

1. **预处理** (`preproc.rs`): 处理 `#include`、`#define`、`#ifdef` 等,展开宏
2. **解析** (`parser.rs`): 递归下降解析器,生成 AST,同时进行类型检查
3. **代码生成** (`codegen/x86_64.rs`): 遍历 AST,直接发射 x86_64 机器码字节
4. **链接** (`link/`): 生成 ELF 可执行文件或目标文件

## 3. 类型系统

### 3.1 Type 枚举 (`src/ast.rs:973-1008`)

```rust
pub enum Type {
    Void,
    Bool,
    Char, SChar, UChar, Char8, Char16, Char32,
    Short, UShort,
    Int, UInt,
    Long, ULong,
    LongLong, ULongLong,
    Int128, UInt128,           // __int128
    Float, Double, LongDouble,
    Fract, Accum,              // 定点数
    Complex(Box<QualType>),    // _Complex float/double
    Imaginary(Box<QualType>),  // _Imaginary
    Ptr(Box<QualType>),
    Array(Box<QualType>, ArraySize),
    Vector(Box<QualType>, usize),  // SIMD 向量
    Struct(String, Arc<Vec<Field>>, usize),  // (名称, 字段, 对齐)
    Union(String, Arc<Vec<Field>>, usize),
    Enum(String, Arc<Vec<(String, i64)>>, Option<Box<QualType>>),
    Func(Box<QualType>, Vec<QualType>),
    Named(String),             // typedef 名称
    Atomic(Box<QualType>),     // _Atomic(T)
}
```

### 3.2 Field 结构 (`src/ast.rs:1010-1027`)

```rust
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub offset: usize,          // 字节偏移(位域: 存储单元起始字节)
    pub bit_width: Option<u32>, // 位域宽度(None = 普通字段)
    pub bit_offset: u32,        // 位域: 存储单元内位偏移
    pub quals: QualifierFlags,
    pub ptr_quals: Vec<QualifierFlags>,
    pub field_align: Option<usize>,
}
```

### 3.3 类型转换

**整数提升** (`parser.rs:1036-1044`):
- `char`/`short` → `int`
- `unsigned char`/`unsigned short` → `int`

**通常算术转换** (`parser.rs:1021-1068`):
```rust
fn usual_arith_type(a: &Type, b: &Type) -> Type {
    // 1. 整数提升
    // 2. 类型秩比较: Int < Long < LongLong < Int128
    // 3. 无符号类型优先
    // 4. 同秩时: 有符号无法表示无符号全部值 → 转无符号
}
```

**浮点转换** (`parser.rs:8793-8800`):
- `float` + `double` → `double`
- 任意 + `long double` → `long double`

**复数转换** (`parser.rs:8761-8792`):
- 复数算术保持复数类型
- `real ± imag` → `complex`
- `imag * imag` → `real`(负值)

### 3.4 代码生成中的类型处理

**整数提升代码生成** (`codegen/x86_64.rs:14297-14318`):
```rust
fn gen_int_promote(&mut self, dst: u8, ty: &Type) {
    match ty {
        Type::Char | Type::SChar | Type::Bool => {
            // movsx dst, dst_low8 (符号扩展)
        }
        Type::Short => {
            // movsx dst, dst_low16
        }
        Type::UChar => {
            // movzx dst, dst_low8 (零扩展)
        }
        Type::UShort => {
            // movzx dst, dst_low16
        }
        _ => {} // int/long/ptr 不需要提升
    }
}
```

**返回语句类型转换** (`codegen/x86_64.rs:7314-7390`):
- `float` → `double`: `cvtss2sd`
- `double` → `float`: `cvtsd2ss`
- `float/double` → `int`: `cvttss2si` / `cvttsd2si`
- 小整数类型截断: 符号扩展/零扩展到 64 位

## 4. 代码生成架构 (x86_64)

### 4.1 Codegen 结构体 (`codegen/x86_64.rs:79-200`)

核心状态:
```rust
pub struct Codegen {
    pub code: Vec<u8>,              // 生成的机器码
    pub entry: u64,                 // 入口点地址
    pub target: Target,             // 目标平台配置
    pub labels: HashMap<String, u64>,
    pub rodata: Vec<u8>,            // 只读数据段
    pub data_section: Vec<u8>,      // 已初始化全局变量
    pub bss_size: u64,              // 未初始化全局变量
    
    // 栈帧管理
    frame_size: i32,
    var_map: HashMap<String, (i32, Type)>,  // 变量名 → (偏移, 类型)
    temp_slots: i32,                // 表达式临时槽
    expr_depth: u32,                // 当前表达式嵌套深度
    
    // 函数调用
    func_param_types: HashMap<String, Vec<Type>>,
    variadic_funcs: HashMap<String, bool>,
    
    // 嵌套函数
    nested_funcs: HashMap<String, Vec<(String, i32)>>,
    static_chain_offset: i32,
    upvar_offsets: HashMap<String, i32>,
    
    // 位域
    bf_rmw_depth: u32,              // read-modify-write 嵌套深度
    
    // 向量
    vec_temp_off: i32,              // 向量临时槽基址
    
    // 可重定位模式
    pub relocatable: bool,
    pub symbols: Vec<ElfSym>,
    pub relocations: Vec<ElfRela>,
}
```

### 4.2 栈帧布局

```
高地址
├─ 参数传递区(调用方栈)
├─ 返回地址
├─ 保存的 r13
├─ 被调用者保存寄存器(rbx, r12, r14, r15)  [r13-16..r13-40]
├─ 表达式临时槽(temp_slots * 8)              [r13-48..r13-(48+temp_slots*8)]
├─ 局部变量区                                [r13-(48+temp_slots*8+var_bytes)]
├─ 变参寄存器保存区(128+48 字节)             [r13-frame_size..]
├─ 静态链槽(嵌套函数)
├─ 向量临时槽(128 字节)
├─ 位域 RMW 临时槽(32 字节)
└─ Trampoline 空间(256 字节)
低地址 (rsp)
```

**关键点:**
- 使用 `r13` 作为帧指针(而非 `rbp`)
- `r13` 在函数入口保存,函数内保持不变
- 变量通过 `[r13 + offset]` 访问
- 表达式求值使用 `rax`、`rcx`、`rdx` 等临时寄存器

### 4.3 主入口函数

**gen_stmt** (`codegen/x86_64.rs:7207`):
```rust
fn gen_stmt(&mut self, s: &Stmt) {
    match s {
        Stmt::Return { val, .. } => { /* 返回值处理 */ }
        Stmt::Expr { e, .. } => { self.gen_expr(e, 0); }
        Stmt::If { cond, then_, else_ } => { /* 条件跳转 */ }
        Stmt::While { cond, body } => { /* 循环 */ }
        Stmt::For { init, cond, step, body } => { /* 循环 */ }
        Stmt::Switch { expr, cases } => { /* 跳转表 */ }
        // ...
    }
}
```

**gen_expr** (`codegen/x86_64.rs:7851`):
```rust
fn gen_expr(&mut self, e: &Expr, dst: u8) {
    match e {
        Expr::Int { v, .. } => { self.mov_imm(dst, *v); }
        Expr::Float { v, .. } => { /* 加载到 xmm0 */ }
        Expr::Ident { name, .. } => { /* 加载变量 */ }
        Expr::Binop { op, l, r, .. } => { /* 二元运算 */ }
        Expr::Unary { op, e, .. } => { /* 一元运算 */ }
        Expr::Call { callee, args, .. } => { /* 函数调用 */ }
        Expr::Assign { l, r, .. } => { /* 赋值 */ }
        Expr::Cast { ty, e, .. } => { /* 类型转换 */ }
        Expr::Member { expr, field, .. } => { /* 结构体成员 */ }
        // ...
    }
}
```

## 5. 函数调用约定 (SysV ABI)

### 5.1 参数分类 (`codegen/x86_64.rs:1744-1950`)

**abi_classes** 函数决定类型的 ABI 分类:

```rust
enum AbiClass {
    NoClass,
    Integer,      // GP 寄存器(rdi, rsi, rdx, rcx, r8, r9)
    Sse,          // XMM 寄存器(xmm0-xmm7)
    SseUp,        // XMM 延续(向量高半部分)
    X87,          // x87 FPU 栈
    X87Up,
    ComplexX87,
    Memory,       // 栈上传递
}
```

**分类规则:**
- 整数/指针: `Integer`
- `float`/`double`: `Sse`
- `long double`: `Memory` (16 字节栈传)
- `_Complex float/double`: `Sse` (各分量)
- `_Complex long double`: `Memory`
- 结构体/联合体: 递归分类各字段,合并结果
- 向量: `Sse` + `SseUp`
- >16 字节或无法放入寄存器: `Memory`

**合并规则** (`merge_classes`, line 1906):
- `Integer` + `Sse` → `Memory`
- `Sse` + `SseUp` → `SseUp`
- 含 `Memory` → 整体 `Memory`

### 5.2 函数调用生成 (`emit_sysv_call`, line 6775)

**流程:**
1. **参数分类**: 对每个参数调用 `abi_classes`
2. **寄存器分配**: 按 GP/XMM/栈分配参数位置
3. **暂存区分配**: 计算需要的暂存区大小
4. **参数求值**: 按顺序求值每个参数,存入暂存区
5. **寄存器加载**: 从暂存区加载到目标寄存器
6. **调用**: `call` 指令
7. **清理**: 恢复栈指针

**关键代码** (line 6788-6836):
```rust
for (arg_idx, arg) in args.iter().enumerate() {
    let effective_ty = /* 参数类型,数组衰减为指针 */;
    let classes = Self::abi_classes(&effective_ty);
    let need_gp = classes.iter().filter(|c| **c == AbiClass::Integer).count();
    let need_fp = classes.iter().filter(|c| matches!(c, AbiClass::Sse | AbiClass::SseUp)).count();
    
    if gp + need_gp > 6 || fp + need_fp > 8 {
        // 栈上传递
        locs.push(ArgLoc::Stack(stack));
        stack += size;
    } else {
        // 寄存器传递
        for class in classes {
            match class {
                AbiClass::Integer => arg_locs.push(ArgLoc::Gp(gp_regs[gp])),
                AbiClass::Sse => arg_locs.push(ArgLoc::Xmm(fp)),
                // ...
            }
        }
    }
}
```

### 5.3 聚合体参数传递

**结构体/联合体** (line 6917-7052):
- ≤16 字节: 按 8 字节块分拆到 GP/XMM 寄存器
- >16 字节: 整个结构体在栈上传递
- `_Complex float/double`: 特殊处理,使用 XMM 寄存器

**CompoundLit** (复合字面量, line 7006-7033):
```rust
// gen_expr 返回地址在 rax
// 从 [rax + chunk*8] 加载各块
for (chunk, loc) in arg_locs.iter().enumerate() {
    self.emit(&[0x48, 0x8B, 0x41, disp]); // mov rax, [rcx+disp]
    match *loc {
        ArgLoc::Stack(off) => self.store_rsp_qword(0, off),
        _ => { /* 暂存到 staging area */ }
    }
}
```

### 5.4 返回值约定

**小返回值** (≤8 字节):
- 整数/指针: `rax`
- `float`/`double`: `xmm0`
- `long double`: `st(0)` (x87 栈)

**大返回值** (>8 字节):
- 结构体/联合体: `rax` + `rdx` (低 8 字节 + 高 8 字节)
- 向量: `rax` + `rdx`
- >16 字节: 调用方提供缓冲区(`rdi` 指向),被调方写入

**内存返回** (`needs_memory_return`, line 1617):
```rust
fn needs_memory_return(ty: &Type) -> bool {
    Self::abi_classes(ty) == vec![AbiClass::Memory]
}
```

## 6. 可变参数处理 (va_arg)

### 6.1 va_list 结构 (SysV ABI)

```c
typedef struct {
    unsigned int gp_offset;      // GP 寄存器偏移(0-48)
    unsigned int fp_offset;      // FP 寄存器偏移(48-176)
    void *overflow_arg_area;     // 栈上参数区
    void *reg_save_area;         // 寄存器保存区
} va_list[1];
```

### 6.2 va_start 实现 (`emit_va_start`, line 17036)

```rust
fn emit_va_start(&mut self, ap_name: &str) {
    let ap_off = self.var_offset(ap_name);
    let gp_init = self.variadic_named_gp.min(6) * 8;
    let fp_init = 48 + self.variadic_named_fp * 16;
    
    // 初始化 gp_offset 和 fp_offset
    self.store_stack_imm32(ap_off, gp_init);
    self.store_stack_imm32(ap_off + 4, fp_init);
    
    // overflow_arg_area: 指向栈上第一个变参
    let overflow_off = self.variadic_overflow_offset;
    self.emit_lea_r13(0, overflow_off);
    self.store_stack(0, ap_off + 8);
    
    // reg_save_area: 指向帧底部的寄存器保存区
    self.emit_lea_r13(0, self.variadic_reg_save_offset);
    self.store_stack(0, ap_off + 16);
}
```

### 6.3 va_arg 实现 (`gen_va_arg`, line 17065)

**整数类型**:
```rust
fn gen_va_arg(&mut self, ap_name: &str, arg_ty: &Type, dst: u8) {
    // 检查 gp_offset < 48
    // 是: 从 reg_save_area + gp_offset 加载
    // 否: 从 overflow_arg_area 加载
    
    // 加载 gp_offset
    self.emit(&[0x41, 0x8B, 0x85]);
    self.emit(&ap_off.to_le_bytes());
    self.emit(&[0x83, 0xF8, 0x30]); // cmp $48, %eax
    self.jmp_rel32(&overflow, JumpKind::Jge);
    
    // reg_save_area + gp_offset
    self.emit(&[0x49, 0x8B, 0x8D]);
    self.emit(&(ap_off + 16).to_le_bytes());
    // 按类型大小加载: 1/2/4/8 字节
    
    // 更新 gp_offset += step
    self.emit(&[0x41, 0x83, 0x85]);
    self.emit(&ap_off.to_le_bytes());
    self.emit(&step.to_le_bytes()[..1]);
}
```

**浮点类型** (`gen_va_arg_fp`, line 17123):
- 检查 `fp_offset < 176`
- 从 `reg_save_area + fp_offset` 加载到 `xmm0`
- 使用 `movss`/`movsd`

**Long double** (`gen_va_arg_long_double`, line 17177):
- 始终从 `overflow_arg_area` 读取(栈上传递)
- 读取 8 字节 double,转换为 x87 格式

## 7. 位域处理

### 7.1 位域存储

位域字段在 `Field` 结构中表示:
```rust
pub struct Field {
    pub offset: usize,          // 存储单元起始字节
    pub bit_width: Option<u32>, // 位域宽度
    pub bit_offset: u32,        // 存储单元内位偏移
}
```

### 7.2 位域读取

**流程:**
1. 加载包含位域的容器(1/2/4/8 字节)
2. 右移 `bit_offset` 位
3. 掩码提取 `bit_width` 位
4. 符号扩展(有符号位域)

### 7.3 位域写入 (Read-Modify-Write)

**gen_bitfield_inc_dec** (line 12938):
```rust
fn gen_bitfield_inc_dec(&mut self, ...) {
    // 1. 获取容器地址,保存到帧槽
    // 2. 加载容器,提取位域值
    // 3. 计算新值(inc/dec)
    // 4. 写回: RMW 操作
    //    - 清除目标位
    //    -  shifts 新值到正确位置
    //    -  OR 操作合并
    //    -  存储容器
    // 5. 设置返回值(前缀/后缀)
}
```

**关键:** 使用 `bf_rmw_depth` 跟踪嵌套深度,为每级 RMW 分配独立的帧临时槽。

## 8. 嵌套函数与 Trampoline

### 8.1 嵌套函数支持

GCC 扩展支持嵌套函数:
```c
int f1(void) {
    int i = 0;
    int f2(int *p) {  // 嵌套函数
        i = 1;        // 访问外层变量
        return *p + 1;
    }
    return f0(f2, &i);
}
```

### 8.2 静态链传递

**调用约定:**
- 嵌套函数通过 `r10` 寄存器接收静态链(指向父帧)
- 调用嵌套函数时,调用方设置 `r10 = r13`(当前帧指针)

**代码生成** (line 7165-7167):
```rust
if self.nested_funcs.contains_key(&effective_name) {
    self.emit(&[0x4C, 0x89, 0xEA]); // mov r10, r13
}
self.call_label(&resolved_name);
```

### 8.3 Upvar 访问

嵌套函数通过静态链访问外层变量:
```rust
// 在父帧中保存静态链
if f.is_nested {
    self.static_chain_offset = /* 计算偏移 */;
    self.emit(&[0x49, 0x89, 0x95]); // mov [r13+off], r10
}

// 在嵌套函数中恢复静态链
// 通过 upvar_offsets 映射访问外层变量
```

### 8.4 Trampoline 生成

Trampoline 是运行时生成的代码,用于将函数指针转换为嵌套函数调用:
```rust
// 预留 256 字节 trampoline 空间
let tramp_reserve = 256;

// Trampoline 代码模板:
// 1. 加载静态链到 r10
// 2. 跳转到实际函数地址
```

## 9. 向量与 SIMD 操作

### 9.1 向量类型

```rust
Type::Vector(Box<QualType>, usize)  // (元素类型, 元素个数)
```

例如: `typedef int v4si __attribute__((vector_size(16)));` → `Vector(Int, 4)`

### 9.2 向量 ABI 分类

**classify_field** (line 1875-1888):
```rust
Type::Vector(_, _) => {
    // 首 eightbyte: Sse
    // 后续 eightbyte: SseUp (向量延续)
    let start = base / 8;
    let end = (base + size.saturating_sub(1)) / 8;
    for i in start..=end {
        let cls = if i == start { AbiClass::Sse } else { AbiClass::SseUp };
        classes[i] = Self::merge_classes(classes[i], cls);
    }
}
```

### 9.3 向量参数传递

**emit_sysv_call** (line 6890-6916):
```rust
if matches!(effective_ty, Type::Vector(_, _)) {
    // 1. 取向量数据地址
    if !self.gen_lvalue_addr(arg, 0) {
        // rvalue: 物化到向量临时槽
        self.gen_expr(arg, 0);
        let toff = self.vector_temp_base(0, effective_ty.size());
        self.emit_lea_r13(0, toff);
    }
    // 2. 逐 8 字节块加载到 rcx,暂存
    for (chunk, loc) in arg_locs.iter().enumerate() {
        self.emit(&[0x48, 0x8B, 0x48, disp]); // mov rcx, [rax+disp]
        match *loc {
            ArgLoc::Stack(off) => self.store_rsp_qword(1, off),
            _ => { /* 暂存到 staging area */ }
        }
    }
}
```

### 9.4 向量运算

向量运算按元素逐个执行:
- 加法: `paddd`/`paddq` (整数) / `addps`/`addpd` (浮点)
- 减法: `psubd`/`psubq` / `subps`/`subpd`
- 乘法: `pmulld` / `mulps`/`mulpd`
- 比较: `pcmpeqd`/`pcmpgtd` / `cmpps`/`cmppd`

## 10. 复数操作

### 10.1 复数类型

```rust
Type::Complex(Box<QualType>)  // _Complex float/double/long double
```

存储为 `(real, imag)` 对:
- `_Complex float`: 8 字节(4+4)
- `_Complex double`: 16 字节(8+8)
- `_Complex long double`: 32 字节(16+16)

### 10.2 复数 ABI 分类

**leaf_abi_class** (line 1945-1947):
```rust
Type::Complex(inner) if matches!(&inner.base, Type::LongDouble) => AbiClass::Memory,
Type::Complex(_) => AbiClass::Sse,  // float/double: 各分量 SSE
```

### 10.3 复数参数传递

**SSE 复数** (line 6920-6948):
```rust
let is_complex_sse = matches!(&effective_ty, Type::Complex(inner)
    if matches!(&inner.base, Type::Float | Type::Double));

if is_complex_sse {
    // _Complex float: 单 eightbyte, movsd 整体装载
    // _Complex double: 两个 eightbyte, 各 movsd 8 字节
    for (chunk, loc) in arg_locs.iter().enumerate() {
        let chunk_off = (chunk as i32) * inner_sz;
        // movsd xmm0, [rax + chunk_off]
        // 存储到目标位置
    }
}
```

### 10.4 复数运算

**复数加法**:
```rust
// (a + bi) + (c + di) = (a+c) + (b+d)i
// 分别对实部和虚部执行浮点加法
```

**复数乘法**:
```rust
// (a + bi) * (c + di) = (ac-bd) + (ad+bc)i
```

**复数返回值** (`gen_complex_sse_return`):
- `_Complex float/double`: 实部在 `xmm0`,虚部在 `xmm1`

## 11. 标签地址与 Computed Goto

### 11.1 &&label 语法

GNU 扩展支持取标签地址:
```c
void *ptr = &&label;
goto *ptr;

label:
    // ...
```

### 11.2 LabelAddr 表达式

```rust
Expr::LabelAddr { label: Ident, span: Span }
```

### 11.3 代码生成 (`gen_expr`, line 9495-9503)

```rust
Expr::LabelAddr { label, .. } => {
    // LEA dst, [RIP + disp32]
    let rex = 0x48 | if dst >= 8 { 0x44 } else { 0 };
    let modrm = 0x05 | ((dst & 7) << 3);
    self.emit(&[rex, 0x8D, modrm]);
    self.emit(&[0, 0, 0, 0]); // disp32 占位符
    
    // 记录延迟解析
    self.label_addr_refs.push((disp32_pos, label_name));
}
```

**延迟解析** (line 2937):
```rust
// 函数体生成完毕后,解析所有标签地址引用
for (disp32_pos, label_name) in self.label_addr_refs.drain(..) {
    if let Some(&label_addr) = self.labels.get(&label_name) {
        let disp = (label_addr as i64 - (disp32_pos + 4) as i64) as i32;
        self.code[disp32_pos..disp32_pos+4].copy_from_slice(&disp.to_le_bytes());
    }
}
```

### 11.4 Computed Goto

```rust
Stmt::GotoIndirect { expr, .. } => {
    self.gen_expr(expr, 0); // rax = 标签地址
    self.emit(&[0xFF, 0xE0]); // jmp rax
}
```

## 12. 测试基础设施

### 12.1 Torture 测试套件

**测试位置**: `tests/gcc-torture/gcc/testsuite/gcc.c-torture/execute/`

**测试脚本**: `scripts/torture_execute_test.sh`

**流程:**
1. 编译每个 `.c` 文件: `dcc -std=gnu11 -lm -o exe src.c`
2. 运行可执行文件,超时 10 秒
3. 检查结果:
   - `PASS`: 退出码 0
   - `RUNTIME_FAIL`: 非零退出码
   - `TIMEOUT`: 超时
   - `COMPILE_FAIL`: 编译失败

**当前状态**: **1489 通过, 208 失败**

### 12.2 Diff 测试

**工具**: `tools/diff_test/diff_test.sh`

**功能**: 比较 dcc 和 gcc 编译输出的符号表和重定位表

**流程:**
1. 用 dcc 和 gcc 分别编译测试用例
2. 用 `readelf` 提取符号表和重定位表
3. 比较差异

### 12.3 失败测试分类

根据测试名称和最近的修复提交,失败主要集中在:

1. **va_arg 相关**: `stdarg-*.c`, `va-arg-*.c`
   - va_list 作为参数传递
   - 复杂的 va_arg 使用模式

2. **结构体操作**: `struct-*.c`, `20010409-1.c`
   - 结构体赋值
   - 结构体作为参数/返回值
   - 嵌套结构体

3. **位域操作**: `bitfield-*.c`
   - 位域增减操作
   - 位域掩码处理

4. **复数操作**: `complex-*.c`
   - 复数算术
   - 复数参数传递

5. **向量/SIMD**: `simd-*.c`, `scal-to-vec*.c`
   - 向量运算
   - 标量到向量转换

6. **嵌套函数**: `20000822-1.c`
   - Trampoline 生成
   - 静态链传递

7. **类型转换**: `pr67929_1.c` (最近修复)
   - float/double 到 int 的转换
   - 整数提升

## 13. 关键代码路径

### 13.1 函数调用完整流程

```
Expr::Call { callee, args, .. }
  ↓
gen_expr (line 9299)
  ↓
emit_sysv_call (line 6775)
  ├─ abi_classes (line 1744) - 分类每个参数
  ├─ 计算寄存器/栈分配
  ├─ 求值每个参数,存入暂存区
  ├─ 加载到目标寄存器
  ├─ call 指令
  └─ 清理栈
  ↓
返回值处理
  ├─ 整数/指针: rax
  ├─ float/double: xmm0
  ├─ 结构体 ≤16B: rax[:rdx]
  └─ 结构体 >16B: sret 缓冲区
```

### 13.2 结构体赋值完整流程

```
Expr::Assign { l, r, .. }  // struct_var = struct_expr
  ↓
gen_expr (line 8319)
  ├─ 检查是否为向量/聚合体
  ├─ 生成右侧表达式
  │   ├─ Ident: 加载地址
  │   ├─ CompoundLit: 生成到栈上,返回地址
  │   └─ Call: 调用函数,结果在 rax[:rdx]
  ├─ 逐 8 字节块复制
  │   for chunk in 0..chunks {
  │       load [src + chunk*8] to rax
  │       store rax to [dst + chunk*8]
  │   }
  └─ 处理对齐和重叠
```

### 13.3 位域写入完整流程

```
Expr::Assign { l: Member { bit_width, bit_offset, .. }, r, .. }
  ↓
gen_expr (line 12938)
  ├─ gen_bitfield_inc_dec (如果是 inc/dec)
  │   ├─ 加载容器
  │   ├─ 提取位域值
  │   ├─ 计算新值
  │   └─ RMW 写回
  ├─ 普通赋值:
  │   ├─ 加载容器到 rcx
  │   ├─ 清除目标位: rcx &= ~(mask << bit_offset)
  │   ├─ 加载新值到 rdx
  │   ├─ shifts: rdx <<= bit_offset
  │   ├─ 合并: rcx |= rdx
  │   └─ 存储容器
  └─ 处理有符号/无符号
```

## 14. 常见问题与修复方向

### 14.1 当前失败模式分析

根据最近修复的提交和失败测试,主要问题集中在:

1. **类型转换不完整**
   - float/double ↔ int 转换 (最近修复: `2afd511`)
   - 整数提升后截断 (最近修复: `5327db6`, `fc26a72`)
   - 返回语句中的类型转换

2. **结构体操作**
   - 结构体赋值 (`ce137a6`: struct assignment from dereferenced pointer)
   - 结构体参数传递 (`6c38472`: compound literal passed as struct argument)
   - 结构体返回值 (`00a1fbe`: struct-return call assigned to global variable)

3. **位域处理**
   - 位域增减 (`b00e431`: bitfield increment/decrement on global struct members)
   - 位域掩码 (`3dbdc75`: bitfield mask sign-extension)

4. **va_arg 实现**
   - va_list 传递 (`083139d`: long double va_arg passing)
   - 寄存器保存区管理

5. **向量/复数**
   - 向量运算 (`1afaced`: vector unary Neg and BitNot)
   - 复数初始化 (`db27cef`: global complex variable initialization)

### 14.2 建议修复优先级

**高优先级** (影响多个测试):
1. va_arg 完整实现(特别是 va_list 作为参数)
2. 结构体赋值和参数传递的边界情况
3. 类型转换完整性(特别是隐式转换)

**中优先级**:
4. 位域操作的 RMW 正确性
5. 复数/向量运算
6. 嵌套函数 trampoline

**低优先级**:
7. 特定 GCC 扩展的兼容性
8. 优化相关的问题

### 14.3 调试建议

1. **使用 diff_test**: 比较 dcc 和 gcc 的输出,定位差异
2. **最小化测试用例**: 提取失败测试的核心代码
3. **检查 ABI 分类**: 使用 `abi_classes` 验证参数/返回值分类
4. **跟踪栈帧**: 检查变量偏移和栈帧布局
5. **验证类型转换**: 确保所有隐式转换都正确生成代码

## 15. 总结

DCC 是一个功能强大的单遍 C 编译器,已经实现了大部分 C11 特性和 GNU 扩展。当前的 208 个失败测试主要集中在:

- **类型系统**: 隐式转换、整数提升、浮点转换
- **聚合体操作**: 结构体/联合体的赋值、参数传递、返回值
- **特殊特性**: va_arg、位域、嵌套函数、向量/复数

代码架构清晰,主要逻辑集中在 `codegen/x86_64.rs` 的 33,512 行代码中。关键函数包括:
- `gen_expr`: 表达式代码生成
- `gen_stmt`: 语句代码生成
- `emit_sysv_call`: 函数调用
- `abi_classes`: ABI 分类
- `gen_va_arg`: 可变参数处理

通过系统性地修复上述问题,可以显著提高 torture 测试的通过率。
