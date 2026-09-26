# va_arg 已知失败用例（XFAIL）

这里放**已定位但尚未修复**、且有 gcc oracle 的复现器，与 `../va_arg/`
（必须全绿，由 `scripts/dcc_gcc_diff.sh tests/fixtures/c/va_arg/*.c` 全量扫）
分开，使那条命令的绿/红语义保持纯净。跨 TU 的绿灯用例在 `../va_arg_mix/`。

| 文件 | 症状 | 已定位的缺陷 |
|---|---|---|
| `vla_agg_via_va_arg.c` | gcc 退出 0，dcc SIGSEGV | D8：①`d.data[i]` 用 `lea -0x78(%r13)` 把 VLA **元数据槽地址**当数组基址（应为 `mov`，取槽里存的分配基址）；②溢出区装载长度用编译期回退值 `mov $0x8,%rcx`，而非 VLA 运行时尺寸 |

（`ld80_vararg_*`（D9）与 `ld80_namedparam_*`（D10）均已修复并移入
`../va_arg_mix/` —— 两者四个方向全绿。D10 的第 3 点（`long double` 返回值
按 SysV 应在 st(0)，旧代码却当 sret 发 `movq %xmm0,(%rdi)`）的根因是
`needs_memory_return` 把 X87 与 Memory 归为一类；参数侧与返回侧对 X87 的
结论**相反**（参数走内存、返回走 st(0)），修时只能拆判据而不能改
`classes_force_mem` 本身。）

收录规则（每条都要满足，否则红灯无法归因）：

1. **必须有 gcc 参照**。原 `../va_arg/vla_struct.c` 用
   `struct {...} first = va_arg(ap, typeof(first));`，GCC 直接拒绝编译
   （"variable-sized object may not be initialized"），只在 dcc 上跑出 SIGSEGV，
   等于没有 oracle —— 已改写成先声明后赋值的形式并移入本目录。
2. 判据走退出码，且不得被 8 位截断伪装成 0（逐参数占位时尤其注意 `1 << 8`）。
3. 文件头要写清"objdump 复核到的实际错误编码"，不能只写"结果不对"。
   注意 `dcc -S` 不可用于判读机器码，必须 objdump。
4. **调用约定类缺陷必须跳编译器**。D9（变参 `long double`）就是反例教材：
   dcc caller 与 dcc callee 共用一个私有约定（"8 字节 double + 8 字节零"），
   所以它是**自洽的错** —— 单 TU 差分、本目录全量扫、cargo 夹具全部通过，
   只有混合链接能照出来。反之，修这类缺陷时两侧必须同批改，
   只改一侧会立即破坏 dcc↔dcc。

跑本目录用例时注意：`scripts/dcc_gcc_diff.sh` 默认用 `target/release/dcc`，
改完代码不重链会得到上一个版本的假红/假绿（已踩过两次）。要么
`cargo build --release`，要么显式 `DCC=target/debug/dcc bash scripts/...`。
