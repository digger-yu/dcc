# va_arg 与 long double 问题彻底解决方案

## 问题背景
9.5 发现当前 va_arg 实现在复杂场景下存在问题，以及 long double 不满足完整 SysV ABI 语义，本次方案旨在彻底解决：

1. 混合整数/浮点结构体的 va_arg 读取不完整
2. va_arg 聚合参数的入口路径不一致
3. va_copy 目前没有看到真正的状态复制实现（已修复，仅做验证）
4. va_list 传参的数组退化语义仍然脆弱
5. long double 实际实现不是完整 SysV 语义

## 总体设计原则
- 严格遵循 SysV ABI 规范（AMD64 版本）
- 统一所有后端的 va_list 语义模型
- 保持现有 API 兼容，不破坏已有功能
- 完整覆盖所有测试场景，增强集成测试

---

## 问题 1 & 2：混合整数/浮点结构体读取与聚合参数入口路径问题

### 当前问题分析

当前在 `gen_va_arg_struct` ([x86_64.rs](file:///root/dcc/src/codegen/x86_64.rs#L23960-L24028)) 中，处理聚合参数（结构体/联合体）时：
1. 仅 `Integer` 和 `Sse`/`SseUp` 会在寄存器保存区路径被处理，其他 ABI 分类会被跳过
2. 混合类型结构体若部分字段落在寄存器保存区，部分被跳过 → 读取不完整
3. 入口对齐仅处理 overflow 路径，寄存器保存区路径未对齐 → 地址错误

### 解决方案

#### 步骤 1：完善 ABI 分类遍历，所有类型都处理
在 `gen_va_arg_struct` 寄存器保存区路径，遍历所有 eightbyte 分类：

| ABI 分类 | 处理方式 |
|---------|---------|
| `Integer` | 从 gp_offset + rax*8 读取 8 字节到 r9，写入目标地址对应偏移 |
| `Sse`/`SseUp` | 从 fp_offset + rdx*16 读取 8 字节（double）或 4 字节（float）到 xmm0，movq 写入目标地址 |
| `X87`/`X87Up`/`ComplexX87`/`Memory` | 此类分类存在时整体已经走 overflow 路径，不会到寄存器路径，所以这里不可能出现，断言即可 |

修改代码位置：[x86_64.rs L23996-L24028](file:///root/dcc/src/codegen/x86_64.rs#L23996-L24028)，在循环中增加对所有分类的处理，而非跳过。

#### 步骤 2：寄存器保存区路径入口对齐
SysV ABI §3.5.7 要求聚合参数在寄存器保存区也需要对齐到其对齐要求。
当前仅 overflow 路径做了对齐，需要在寄存器路径也增加对齐判断：

```rust
// 在进入寄存器路径前，和 overflow 路径一样做对齐检查
if align > 8 {
    // gp_offset 要对齐到 align
    let align_mask = !(align - 1);
    rax = (rax + align - 1) & align_mask;
    // 如果 gp 调整后溢出，需要跳转到 overflow
    if rax > 48 - 8 * n_gp {
        jmp overflow;
    }
    // 同理 fp_offset 也需要对齐
    if n_fp > 0 {
        rdx = (rdx + align - 1) & align_mask;
        if rdx > 176 - 16 * n_fp {
            jmp overflow;
        }
    }
}
```

#### 步骤 3：统一溢出检查顺序
当前先 gp 溢出检查，再 fp，调整顺序为对齐后再溢出检查，保证对齐后溢出判断正确。

#### 步骤 4：所有后端同步对齐逻辑
对于 aarch64/riscv64/arm/mips，需要检查是否对齐逻辑缺失，同步对齐实现。

---

## 问题 3：va_copy 状态复制（验证结论：已修复，仅需补充文档/测试）

当前 x86_64 实现已经正确：
- 完整复制 va_list 的全部字段（gp_offset, fp_offset, reg_save_area, overflow_arg_area）
- 正确处理地址规则：区分 dest/src 是 va_list[1] 还是 va_list*
- 使用 rep movsb 复制整个结构

仅需：
1. 同步所有后端到 x86_64 的实现方式
2. 现有集成测试已覆盖 [va_copy.c](file:///root/dcc/tests/fixtures/c/va_arg/va_copy.c)、[va_copy_advanced.c](file:///root/dcc/tests/fixtures/c/va_arg/va_copy_advanced.c)，不需要新增测试

---

## 问题 4：va_list 传参数组退化语义

### 当前问题分析
- C 语言规定 `typedef va_list[1];` 声明 va_list，当作为函数参数传递时会退化成为指针
- 当前仅 x86_64 在 `__builtin_va_copy` 中正确处理数组退化和地址计算
- 其他后端处理简化，前端语法解析在复杂场景（结构体嵌套 va_list、多层指针、取地址）可能不正确
- 参数传递时数组退化到指针的规则没有统一全链路实现

### 解决方案

#### 前端：统一处理 va_list 参数退化
在 parser 参数声明阶段 ([parser.rs](file:///root/dcc/src/parser.rs)):

当参数类型为数组，且元素类型是 `__builtin_va_list`（即 `va_list` 形参），自动退化到 `__builtin_va_list *` 类型，和普通数组参数一致。

保证所有场景下，va_list 形参都是指针类型，和 C 语言语义一致。

#### codegen：统一所有后端 va_start/va_copy 地址规则
所有后端需要遵循同一个地址规则：

- `va_start(ap, last_named_arg)`: ap 是 va_list 数组 → 取 ap[0] 的地址初始化
- `va_copy(dest, src)`:
  - 如果 dest/src 是指针类型（已退化），直接取值
  - 如果是数组类型（局部变量声明），取元素地址
  - 复制整个 va_list 结构从 src 地址到 dest 地址

所有后端对齐 x86_64 的实现逻辑，保证一致性。

#### 测试：补充复杂退化场景
新增集成测试：
- 函数参数传递 va_list 到子函数
- 结构体嵌套 va_list 成员，做 va_copy
- 传递 &ap 到子函数，子函数内部使用

---

## 问题 5：long double 完整 SysV ABI 实现

### 当前问题分析

| 层级 | 问题 |
|-----|------|
| **前端语法/常量解析** | long double 常量被解析为 f64，丢失精度 |
| **AST/类型系统** | Type::LongDouble 大小仍然是 8 字节，实际 SysV AMD64 要求 16 字节对齐 16 字节 |
| **语义分析** | 无大问题，大小正确后布局自然正确 |
| **ABI 参数分类** | 分类正确为 X87，但是大小错误会影响 eightbyte 计数错误 |
| **IR lowering** | 无大问题，只要类型大小对齐对了，IR 不变 |
| **函数序言（prologue）** | 变参函数保存 xmm 寄存器到 reg_save_area，XMM 保存是 16 字节每个，和 long double 无关 |
| **va_arg codegen** | 当前按 8 字节读取，需要改为 16 字节对齐读取 |
| **函数调用 codegen** | 压栈需要对齐到 16 字节 |

### 解决方案，分步骤重构：

#### 步骤 1：修正 Type::LongDouble 大小和对齐
修改 [ast.rs](file:///root/dcc/src/ast.rs):

```rust
// 修改 long double 大小：
Type::LongDouble => 16, // 原来是 8
// 对齐：
align_of(Type::LongDouble) => 16, // 原来是 8
```

影响：
- 结构体布局自动修正，含 long double 的结构体大小/偏移自动正确
- ABI 分类 eightbyte 分割自动正确

#### 步骤 2：重构常量解析，支持 80-bit 扩展精度
修改 [const_eval.rs](file:///root/dcc/src/const_eval.rs):

当前所有浮点常量统一存为 f64，需要改为存储原始字节和格式：
1. 新增枚举 `FloatBits` 区分：
   - `F32(u32)` 32-bit float
   - `F64(u64)` 64-bit double
   - `F80(u64[2])` 80-bit x87 long double
   - `F128(u64[2])` 128-bit IEEE quad

2. 解析阶段：
   - 对于 `f` 后缀 → f32
   - 无后缀 → f64
   - `l` 后缀 → 解析为 80-bit x87 扩展精度，存为 10 字节（对齐 16 字节后高 6 字节补零）

3. 常量运算：
   - 运算时将 F80 转换为 f64 计算（保持兼容性，用户不做运算不丢失精度）
   - 只有常量传递 → 保留原始 80-bit 比特

4. 代码生成时：
   - va_arg 读取 long double 从内存加载 16 字节，低 10 字节是有效 x87 值

#### 步骤 3：修正 x86_64 va_arg long double codegen
当前实现 ([x86_64.rs L24330-L24359](file:///root/dcc/src/codegen/x86_64.rs#L24330-L24359)):
- 按 8 字节读取，加载为 double → 需要修改：

SysV ABI 要求：
- long double 对齐 16 字节
- 占用 16 字节，低 10 字节是 x87 80-bit 值
- va_arg 读取时对齐后，fldt 加载到 x87 栈（st0）

修改：
```nasm
; 原来: movsd (%r9), %xmm0  ; 读 8 字节到 xmm0
; 改为: fldt (%r9)         ; 读 10 字节 80-bit 到 x87 栈 st0
; 然后：add r9, 16  ; 总是推进 16 字节，符合对齐要求
```

dcc 内部约定：long double 参数返回在 st0，和 SysV ABI 一致。

#### 步骤 4：修正 ABI 参数分类
当前分类正确，但因为大小改变，eightbyte 计数会自动修正，不需要额外修改。

测试增加：含多个 long double 的结构体 va_arg，验证偏移正确。

#### 步骤 5：所有后端同步 long double 修改
对于其他非 x86_64 后端，根据目标平台 ABI 修正：
- aarch64: long double 也是 128 位，16 字节对齐
- riscv64: 同 aarch64
- arm/mips: 根据平台 ABI 修正大小对齐

---

## 集成测试补充

需要新增/更新测试用例到 `tests/fixtures/c/va_arg/`:

| 测试文件 | 测试内容 |
|---------|---------|
| `struct_mixed_ints_float.c` | 混合多个 int + float 结构体 va_arg |
| `struct_mixed_ld.c` | 含 long double + 整数混合结构体 va_arg |
| `va_list_param_passing.c` | va_list 作为参数传递给子函数，验证退化 |
| `nested_struct_va_copy.c` | 结构体嵌套 va_list，验证 va_copy 正确 |
| `long_double_constant.c` | long double 常量传递和精度验证 |

---

## 实施步骤（TODO）

1. 修复问题 1/2：完善聚合 va_arg 混合类读取和入口对齐 x86_64
2. 同步所有后端聚合读取和对齐逻辑
3. 统一所有后端 va_copy 实现到 x86_64 标准
4. 修复前端 va_list 参数退化规则
5. 修正 Type::LongDouble 大小对齐
6. 重构常量求值支持 80-bit 长双精度
7. 修正 x86_64 va_arg long double codegen
8. 同步所有后端 long double 修改
9. 新增集成测试覆盖所有场景
10. 运行全部测试，验证无回归

---

## 风险和兼容性

- 不改变现有 API，所有用户代码不需要修改
- long double 改变后，含 long double 的结构体内存布局会变化，但这是修复错误，原来布局不符合 SysV ABI
- 常量精度改变后，链接和运行时行为不变，仅精度提升
- 对变参性能影响可忽略，复杂场景才会触发多分类遍历