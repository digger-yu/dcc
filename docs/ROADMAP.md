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
| **M6b** | GCC libiberty（短期） | ✅ 完成 | 远程 125/129 OK；见 `docs/GCC_BUILD_GUIDE.md` |
| **M6c** | 内核全树验收脚本 | ✅ 完成 | modpost_link / kernel_full_check / vmlinux_regress / qemu_boot_matrix；LTO gc-sections |
| **M7** | ARM64 + RISC-V | ⏳ | hello.c 交叉编译,运行 |
| **M8** | C11/17/23 + GNU 扩展 | ✅ 完成 | 位域/原子/SIMD/定点数/柔性数组/内存序 后端已实现+Linux验证; C标准全家族别名支持(c89/c90/c99/c9x/c11/c1x/c17/c18/c23/c2x+gnu变体+iso9899) |
| **M9** | 精准错误诊断 | ✅ 完成 | 错误精确到字符+列+修复建议, GitHub-style 行+指针+下划线格式; 支持 E0001-E0006+W0001 (parser), E0100-E0101 (driver), E0200-E0206+W0200 (preprocessor) |
| **M10** | 跨平台交叉编译 | ⏳ | riscv64-linux-gnu-dcc 命名 |

## 后端能力矩阵 (M8 细分)

| 模块 | 优先级 | 前端(解析) | 后端(代码生成) | 验证测试 |
|------|--------|----------|------------|----------|
| 位域 bitfields | P2 | ✅ `int x : 3` 语法 | ✅ 位偏移+掩码+移位+有符号扩展 | ✅ bitfield_test.c |
| 柔性数组成员 | P2 | ✅ `char data[]` | ✅ sizeof不贡献+偏移正确 | ✅ flex_arr_test.c |
| 向量类型 SIMD | P3 | ✅ `__attribute__((vector_size))` | ✅ 标量逐元素加减乘 | ✅ simd_test.c |
| 定点数类型 | P3 | ✅ `_Fract`/`_Accum` | ✅ 作为整数运算(1.31/16.16) | ✅ fixedpt_test.c |
| 原子操作 | P3 | ✅ `__atomic_*` 内建函数 | ✅ lock前缀+xchg+cmpxchg | ✅ atomic_test.c |
| 原子内存序 | P3 | ✅ `__ATOMIC_*` 宏定义 | ✅ mfence(seq_cst) | ✅ atomic_test.c |
| __builtin__ 语义 | P1 | ✅ expect/constant_p/memcpy 等 | ✅ codegen 层内建函数处理 | ✅ examples/dev/builtins.c |
| __attribute__ 语义 | P1 | ✅ packed/aligned/noreturn/unused | ✅ 解析器静默剥离 | ✅ examples/dev/builtins.c |
| sizeof(expr) 修复 | P2 | ✅ 计算实际表达式类型大小 | ✅ | ✅ examples/dev/builtins.c |
| _Alignof 对齐查询 | P2 | ✅ _Alignof(type)/_Alignof(expr) | ✅ 返回对齐值 | ✅ examples/dev/builtins.c |
| typeof 运算符 | P3 | ✅ typeof(expr/type) 推导类型 | ✅ | ✅ examples/dev/builtins.c |
| __extension__ | P3 | ✅ 解析器直接跳过 | ✅ | ✅ examples/dev/builtins.c |
| 语句表达式 | P3 | ✅ ({...}) 块内最后表达式值 | ✅ | ✅ examples/dev/builtins.c |
| 函数指针调用 | P1 | ✅ 表达式 callee (Box<Expr>) | ✅ 间接调用 call *r11 | ✅ runtime_features.c |
| 逗号运算符 | P2 | ✅ Pratt bp=1 最低优先级 | ✅ 求值 lhs 丢弃返回 rhs | ✅ runtime_features.c |
| VLA 可变长数组 | P2 | ✅ ArraySize::Var(Box<Expr>) | ✅ 动态栈分配+16字节对齐 | ✅ runtime_features.c |
| 整数提升 | P2 | ✅ char/short 隐式提升 | ✅ movsx/movzx 符号/零扩展 | ✅ runtime_features.c |
| 内联汇编 __asm__ | P1 | ✅ __asm__/__asm/asm + volatile/const, 带冒号约束列表 | ✅ no-op 透传 | ✅ runtime_features.c, Linux 内核 devicetable-offsets.c |
| LTO + dead symbol gc | P2 | ✅ `-flto` / `-ffunction-sections` | ✅ `.llvm.lto` + `ld --gc-sections` + objcopy strip | ✅ `cargo test` + remote_feature_check |
| struct 整体赋值 | P1 | ✅ 聚合初始化 | ✅ memcpy 整体拷贝 | ✅ Linux 验证 |
| struct 传参/返回 | P2 | ✅ 函数参数/返回值 | ✅ SysV ABI 寄存器拆分 (<=16B) | ✅ Linux 验证 |
| long double 16B | P3 | ✅ sizeof=16 | ✅ 16字节存储 + SSE 运算 | ✅ Linux 验证 |
| _Complex 复数 | P3 | ✅ _Complex float/double/long double + __complex__/__complex | ✅ sizeof+变量声明+初始化+赋值+返回; 实部存储+虚部清零; >8B 聚合拷贝 | ✅ Linux 验证, 本地测试套 |
| _Decimal 十进制 | P3 | ✅ _Decimal32/64/128 | ✅ 映射到等效 IEEE 浮点 | ✅ Linux 验证 |

### 第一性原理设计决策

- **位域**: 读=加载容器→右移→掩码, 写=读-改-写(CAS风格), 容器类型决定存储单元大小
- **柔性数组**: 零长数组不贡献 struct 大小, 偏移紧跟前一字段, 运行时需配合 malloc
- **SIMD**: 不用 XMM 寄存器, 纯标量逐元素运算, 通过帧底临时区传递结果
- **定点数**: ISO/IEC DTR 18037 嵌入式扩展, _Fract=1.31位 _Accum=16.16位, 复用整数路径
- **原子操作**: x86 TSO 架构下, lock前缀保证原子性, fetch_add用xadd, 其余用cmpxchg循环
- **内存序**: x86 天然 TSO, relaxed=裸操作, acquire/release=编译器屏障, seq_cst=mfence
- **__builtin__**: expect=透传expr, constant_p=返回0, memcpy/memset=转发libc, abs=test+jns+neg
- **__attribute__**: 解析器静默剥离 packed/aligned/noreturn/unused/weak/section 等, 不影响 codegen; 支持声明开头/类型限定符间/函数名后/参数后等多位置属性
- **函数指针**: 支持声明 `void (*fp)(int)`, typedef `typedef int (*initcall_t)(void)`, 参数 `void (*cb)(int)`, 强制转换 `(void (*)(int))0`
- **__asm__ 标签**: 支持 `__asm__("label")` 作为函数/变量符号重命名, 解析器静默跳过
- **typeof**: 解析期推导类型, typeof(expr)→expr.ty(), typeof(type)→type
- **语句表达式**: 执行块内语句, 最后表达式值作为结果留在目标寄存器
- **函数指针调用**: callee 从 Ident 改为 Box<Expr>, 直接调用走 call_label, 间接调用求值到 r11 后 call *r11 (49 FF D3)
- **逗号运算符**: Pratt 解析 bp=1 最低优先级, 调用参数用 parse_expr_bp(2) 排除逗号, 初始化列表同理
- **VLA**: ArraySize::Var 存储运行时表达式, 分配策略: 求值大小→imul元素大小→16字节对齐→sub rsp→保存基址到变量槽
- **整数提升**: gen_int_promote() — char/SChar/Bool→movsx, Short→movsx, UChar→movzx, UShort→movzx
- **内联汇编**: 识别 __asm__/__asm/asm, 跳过 volatile/const, 收集字符串, 跳过冒号分隔输出/输入/clobber 列表, codegen 为 no-op; 支持文件级 asm 声明和函数内 asm 表达式
- **-MD/-MF 依赖文件**: 内核构建系统需要编译器生成 .d 依赖文件, 格式为目标:源; 支持 -MD(启用)/-MF(指定路径)/-MT-MQ(目标名), 无 -MF 时默认输出 <stem>.d
- **struct 传参**: SysV ABI 简化版, <=8B 单寄存器, <=16B 拆双寄存器(rax:rdx), >16B 隐藏指针; 被调端从寄存器恢复到变量槽
- **long double 16B**: 占 16 字节栈空间(低 8 字节 double + 高 8 字节零), 运算用 SSE double 近似, movq 加载低 8 字节
- **_Complex**: Type::Complex(Box<Type>) 包装内部类型, size=inner*2, 支持 __complex__/__complex/_Complex 关键字, 代码生成实部存储+虚部清零, >8B 使用聚合拷贝路径; 标量初始化复杂类型时虚部自动清零
- **_Decimal**: 解析期直接映射到等效类型 (_Decimal32→float, _Decimal64→double, _Decimal128→long double), 无需独立 codegen
- **全局类型注册表**: 头文件缓存到 ~/.dcc/cache/, 键=文件路径哈希+内容哈希(FNV-1a), 值=预处理输出+宏状态, 内容变化时自动失效

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
