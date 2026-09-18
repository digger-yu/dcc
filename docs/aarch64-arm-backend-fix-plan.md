# DCC AArch64/ARM 后端问题修复方案

## 问题总览

| 架构 | 问题描述 | 当前状态 | 严重度 |
|------|----------|----------|--------|
| AArch64 | Complex 类型内部类型转换未处理（float→double 等） | 不完整，存在漏洞 | 中 |
| AArch64 | ldr/str 使用寄存器偏移而非立即数偏移（标注 TODO） | 未实现 | 中 |
| ARM | 聚合类型字段偏移计算使用硬编码而非 aggregate_fields | 部分实现仍有漏洞 | 中 |
| 通用 | cpuid.h / mm_malloc.h / avx512intrin.h 支持 | 未实现 | 低 |

---

## 问题 1：AArch64 Complex 类型内部类型转换未处理

### 当前问题
- 位置：[aarch64.rs:2955](file:///root/dcc/src/codegen/aarch64.rs#L2955)
- 当前行为：Complex → Complex 转换直接加载两个分量，但不处理内部类型不同的转换（如 `float complex` → `double complex`）。
- 影响范围：所有跨精度的复数转换不能生成正确机器码，结果分量精度错误。

### 根因
开发时只覆盖了相同内部类型的转换，没有处理不同类型的分量逐元素转换。

### 修复方案

#### 设计原则
- 遵循 AArch64 ABI：复数返回值放在 `v0` (实部), `v1` (虚部) 寄存器对。
- 分量分别转换，保持返回位置不变。
- 复用现有标量转换路径，减少重复代码。

#### 实施步骤

1. **获取转换信息**
   - 在 `gen_complex_expr` 的 `Cast` 分支，取出源类型 `source_inner` 和目标类型 `target_inner`。
   - 如果 `source_inner == target_inner`，不需要转换，保持当前行为直接返回。

2. **按情况分别处理**
   | 源 → 目标 | 处理 |
   |-----------|------|
   | `float` → `double` | `fcvtl d0, s0` (实部), `fcvtl d1, s1` (虚部) |
   | `double` → `float` | `fcvtn s0, d0` (实部), `fcvtn s1, d1` (虚部) |
   | `int` → `float/double` | 已经处理 (scalar → complex 路径) |
   - AArch64 指令手册：`fcvtl` 将单精度 SIMD 元素转为双精度；`fcvtn` 反之。

3. **测试要点**
   - 添加转换测试用例：
     ```c
     float complex fc = ...;
     double complex dc = (double complex)fc;

     double complex dc2 = ...;
     float complex fc2 = (float complex)dc2;
     ```
   - 验证 objdump 中机器码是否有 `fcvtl`/`fcvtn`。
   - 运行时验证结果精度。

---

## 问题 2：AArch64 ldr/str 寄存器偏移寻址未实现

### 当前问题
- 位置：[aarch64.rs:3742](file:///root/dcc/src/codegen/aarch64.rs#L3742), [aarch64.rs:3744](file:///root/dcc/src/codegen/aarch64.rs#L3744)
- 当前行为：aggregate 复制循环中，偏移已经在寄存器 `x14`，但当前 `emit_ldr_x/emit_str_x` 只支持立即数偏移，硬编码偏移为 0，导致地址错误。
- 影响范围：大结构体/聚合复制循环始终访问偏移 0，复制结果错误。

### 根因
所有现有内存读写函数 (`emit_ldr_x`, `emit_str_x`, `emit_ldr_s`, `emit_str_s`, ...) 只实现了立即数偏移，没有寄存器偏移版本。
AArch64 寻址模式分三种：
1. 立即数偏移 (imm9) — 已实现
2. 寄存器偏移 (base + 索引) — 未实现
3. 立即数偏移带缩放 — 不需要，复合索引已经在 IR 层面计算。

### 修复方案

#### 设计原则
- 所有现有 `emit_*` 函数添加 `reg_offset: Option<u8>` 参数，None 表示不使用寄存器偏移。
- 复用现有操作码编码逻辑，只在寄存器偏移时修改 `sz`/`opc`/`S` 位。

#### AArch64 指令编码对照

| 指令类型 | 立即数偏移 | 寄存器偏移 |
|----------|------------|------------|
| LDR X | `0xF9400000 + (imm << 10) + (Rn << 5) + Rt` | `0xF8600000 + (Roffset << 16) + (Rn << 5) + Rt` |
| STR X | `0xF9000000 + (imm << 10) + (Rn << 5) + Rt` | `0xF8200000 + (Roffset << 16) + (Rn << 5) + Rt` |
| LDR S/W | `0xBD400000 + (imm << 10) + (Rn << 5) + Rt` | `0x38600000 + (Roffset << 16) + (Rn << 5) + Rt` |
| STR S/W | `0xB9400000 + (imm << 10) + (Rn << 5) + Rt` | `0x38200000 + (Roffset << 16) + (Rn << 5) + Rt` |
| LDR D | `0xBD800000 + (imm << 10) + (Rn << 5) + Rt` | `0x7C600000 + (Roffset << 16) + (Rn << 5) + Rt` |
| STR D | `0xBD000000 + (imm << 10) + (Rn << 5) + Rt` | `0x7C200000 + (Roffset << 16) + (Rn << 5) + Rt` |
| LDR Q | `0x3DC00000 + (imm << 10) + (Rn << 5) + Rt` | `0xFC600000 + (Roffset << 16) + (Rn << 5) + Rt` |
| STR Q | `0x3D800000 + (imm << 10) + (Rn << 5) + Rt` | `0xFC200000 + (Roffset << 16) + (Rn << 5) + Rt` |

所有寄存器偏移指令 opcode 都不同，必须区分编码。

#### 实施步骤

1. **修改所有 emit 函数**
   - `fn emit_ldr_x(&mut self, xt: u8, rn: u8, imm: i32, size: usize, reg_off: Option<u8> = None)`
   - 同理修改 `emit_str_x`, `emit_ldr_s`, `emit_str_s`, `emit_ldr_d`, `emit_str_d`, `emit_ldr_q`, `emit_str_q`。
   - 默认 `None` 保持向后兼容，不需要全代码库修改。

2. **按照上表编码正确 opcode**
   - 当 `reg_off.is_some()`，使用寄存器偏移 opcode。
   - `reg_off.unwrap()` 放入 bits 16-20。

3. **修复 aggregate 复制循环**
   - 在 `emit_aggregate_memcpy` 中：
     ```rust
     self.emit_ldr_x(9, src_reg, 0, 8, Some(14));
     self.emit_str_x(9, dst_reg, 0, 8, Some(14));
     ```
   - 这样就不需要手动 add x14，每轮循环直接加载 `[src + x14]`。

4. **测试要点**
   - 大结构体赋值测试（size > 32B，触发 memcpy loop）。
   - 验证 `objdump -d` 生成的机器码使用正确寄存器偏移寻址。
   - 运行测试验证结构体每个字段复制正确。

---

## 问题 3：ARM 聚合类型字段偏移硬编码问题

### 当前问题
- 位置：[arm.rs:1169-1181](file:///root/dcc/src/codegen/arm.rs#L1169-L1181)
- 当前行为：
  1. 数组索引时：假设所有元素大小为 4 字节，硬编码左移 `emit_lsl(0, 0, 0)`，未按实际元素大小缩放索引。
  2. 结构体/联合数组索引同理硬编码。
- 已正确处理：`Expr::Member` 已经使用 `aggregate_fields` 获取正确偏移，不用改。
- 影响范围：非 4 字节元素数组索引地址错误，读取/写入错误内存位置。

### 修复方案

#### 设计原则
- 和 x86_64 对齐，复用 `aggregate_fields` 存储结构体字段布局，数组元素大小从类型信息获取。
- 严格遵循 ARM AAPCS 寻址规则。

#### 实施步骤

1. **修复 Expr::Index 路径**
   - 取出元素类型 `inner`，计算 `elem_size = inner.base.size()`。
   - 如果 `elem_size == 1`: 不需要移位，索引直接就是偏移。
   - 如果 `elem_size == 2`: `lsl #1`.
   - 如果 `elem_size == 4`: `lsl #2`.
   - 如果 `elem_size == 8`: `lsl #3`.
   - 如果 `elem_size > 8`: 用 `mul r0, r0, #elem_size` 缩放。ARM32 立即数支持 8 比特，`elem_size` 总是对齐，可编码。

2. **对齐 Expr::Member 路径**
   - 当前 `Expr::Member` 已经查找 `aggregate_fields`，没问题。
   - 检查当 field_off 为 0 时是否仍然正确，不需要修改。

3. **测试要点**
   - 添加不同元素大小数组索引测试：
     ```c
     char arr[10]; arr[i] → 不缩放 (lsl 0)
     short arr[10]; arr[i] → lsl 1
     int arr[10]; arr[i] → lsl 2
     double arr[10]; arr[i] → lsl 3
     struct S { int x; }; struct S arr[10]; arr[i].x → mul 8 对齐后正确。
     ```
   - 运行时验证读取结果正确。

---

## 新增头文件支持：cpuid.h / mm_malloc.h / avx512intrin.h

### 目标
在 `src/cpp/stdlib/` 添加三个头文件，提供 GCC/Clang 兼容的声明/内建定义。

### 实施方案

#### 1. cpuid.h
- 提供 `__get_cpuid`, `__cpuid` 内建函数声明。
- 内建调用映射到 DCC 内置 cpuid 指令生成，类似 `__builtin__cpuid`。
- 遵循 GCC 接口：
  ```c
  void __cpuid(int leaf, int cpu_info[4]);
  int __get_cpuid(unsigned int leaf, unsigned int *eax, unsigned int *ebx,
                  unsigned int *ecx, unsigned int *edx);
  ```

#### 2. mm_malloc.h
- 提供 `mm_malloc`, `mm_free` 对齐分配。
- 实现：
  ```c
  static inline void* mm_malloc(size_t size, size_t align) {
    // 按照 align 分配，返回对齐地址，可基于 malloc 实现。
  }
  static inline void mm_free(void *ptr);
  ```
- 遵循 glibc/mingw 接口。

#### 3. avx512intrin.h
- 提供 AVX-512  intrinsics 声明和模拟，针对标量运算。
- 当前 DCC 只有 AVX2，所以仅声明类型和基础宏，实际内建由后端逐步完善。

---

## 整体实施计划

### 阶段划分（每个阶段可独立 commit）

1. **阶段 1**：修复 AArch64 Complex 内部转换 → 单元测试覆盖 → 验证回归。
2. **阶段 2**：实现 AArch64 寄存器偏移寻址 → 修复大结构体复制 → 测试验证。
3. **阶段 3**：修复 ARM 数组索引缩放 → 使用正确元素大小 → 测试验证。
4. **阶段 4**：添加三个头文件到 `cpp/stdlib` → 添加简单测试验证包含可用。

### 提交要求
每个阶段修复后运行 `cargo test` 确认无回归，`objdump -d` 验证机器码，再提交。

### 验收标准
- 所有新增测试用例通过。
- 现有测试套件全过，不引入回归。
- 问题点覆盖：
  - Complex float/double 转换机器码正确，结果精度正确。
  - 大 aggregate 复制使用寄存器偏移寻址，复制结果正确。
  - ARM 任意大小元素数组索引地址计算正确。
  - 三个新头文件可以 `#include` 并使用声明。

---

## 风险缓解
- 对 AArch64 指令编码再次核对 ARM 手册，避免 opcode 错误。
- 保持 API 向后兼容，默认参数不破坏现有调用。
- 分阶段实施，每个阶段小 commit 便于回滚。
