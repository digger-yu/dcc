# DCC 路线图 (v0.1)

> suckless 哲学: 先把每一关跑通,再加下一关。绝不一步迈两步。

| 里程碑 | 标题 | 状态 | 核心验证 |
|--------|------|------|----------|
| **M0** | 最小 hello world | ✅ 完成 | `int main(){return 42;}` -> a.out, exit 42 |
| **M1** | C89/99 子集 + 控制流 | ✅ 完成 | examples/fib.c -> fib(10)=55 |
| **M2** | struct/union/enum + 指针 | ✅ 完成 | examples/struct_test.c |
| **M3** | 全局类型注册表 | ⏳ | 二次编译时间 -50% |
| **M4** | 预处理 + builtin | ✅ 完成 | linux/init/main.c 解析成功 |
| **M5** | **自举 (dcc 编 dcc)** | ⏳ | dcc1 == dcc2 |
| **M6** | Linux 内核 benchmark | ⏳ | < GCC 时间, vmlinux 可启动 |
| **M7** | ARM64 + RISC-V | ⏳ | hello.c 交叉编译,运行 |
| **M8** | C11/17/23 + GNU 扩展 | ✅ 完成 | 位域/原子/SIMD/定点数/柔性数组/内存序 后端已实现+Linux验证 |
| **M9** | 精准错误诊断 | ⏳ | 错误精确到字符+列+建议 |
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

### 第一性原理设计决策

- **位域**: 读=加载容器→右移→掩码, 写=读-改-写(CAS风格), 容器类型决定存储单元大小
- **柔性数组**: 零长数组不贡献 struct 大小, 偏移紧跟前一字段, 运行时需配合 malloc
- **SIMD**: 不用 XMM 寄存器, 纯标量逐元素运算, 通过帧底临时区传递结果
- **定点数**: ISO/IEC DTR 18037 嵌入式扩展, _Fract=1.31位 _Accum=16.16位, 复用整数路径
- **原子操作**: x86 TSO 架构下, lock前缀保证原子性, fetch_add用xadd, 其余用cmpxchg循环
- **内存序**: x86 天然 TSO, relaxed=裸操作, acquire/release=编译器屏障, seq_cst=mfence

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
