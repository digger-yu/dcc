# DCC 手工编译 Linux 内核指南

> 目标版本：**Linux 7.1 / x86_64**。本地验收目录：`./`。

## 目标（2026-11-01 改版，2026-11-03 换档）

**11-03 换档**：验收配置由 **allmodconfig** 改为 **localmodconfig**（allmodconfig 规模太大：实测 4969 个 `=y` + 10568 个 `=m`，全量 vmlinux 跑到 1898 个对象仍未进入链接）。localmodconfig 落盘后 **`=y` 1577 个、`=m` 1 个**（相当于 `x86_64_defconfig` 再剔掉本机未加载的模块），PVH 相关开关不变。allmodconfig 保留为**压力项**：`KCONFIG=allmodconfig bash scripts/kernel_pvh_verify.sh --config` 可切回。

| 项 | 旧 | 新 |
|---|---|---|
| 配置 | 定制小配置（738 个 `=y`、0 个 `=m`，共 1489 个内核 `.o`） | **localmodconfig**（`=y` 1577 / `=m` 1；allmodconfig 降为压力项） |
| 验收标准 | 编到链接（`vmlinux`） | **进入用户空间**（QEMU 起内核跑到 init） |
| 编译器 | 内核 TU 用 dcc，宿主工具与链接驱动用 gcc | **一律 dcc**：编译步骤不许出现 gcc，链接也不许起 gcc 进程（走 `ld` 或内置链接器） |

规模与约束（同一台 4 核 / 7 GB 机器）：

- **磁盘是这个目标的主要约束之一**。对象数比小配置多一个量级，而 dcc 当前 `.text` 是 gcc 的
  3-7 倍。本轮先 `bash scripts/cleanup_disk.sh`（默认档，不动 `target/`）把 51 GB 放到 **72 GB**，
  并清掉 947 个失效 `.o`。每完成一轮修件就复跑 `df -h /` 与 `bash scripts/cleanup_disk.sh --dry`；
  最大的一项历来是 `/tmp 杂项`（本轮 8.8 GB）与 `~/.dcc`（3.2 GB）。
  **陷阱**：脚本第 6 项会**整棵清空 `.work_tmp/`**，那里放着全套判据脚本
  （`kwaudit.sh`/`rb.sh`/`kwtraps.sh`/`kwghostscan.sh`/`xsym.awk`/`kwweak*.sh`/`kwund*.sh`），
  跑前必须先 `tar czf /root/worktmp-backup.tgz .work_tmp`，跑完再解回来。
- **并行度 `-j2`**：7 GB 内存下 allmodconfig 的大 TU 会顶到 OOM，`-j4` 曾把机器压到无响应。

四条待决路线（按「最省改动又能达成验收」排序，遇到实证再翻）。**11-03 复核**：1、2 已落地，3 选了 (B) 并正在跑全量验证，4 待验（本配置 `MODULE_SIG`/`MODVERSIONS` 都没开，`OBJTOOL=y` 仍需过）：

1. **[已完成] 宿主工具**：`scripts/dcc-host-wrapper.sh` 现在是 `dcc -c` + **直接驱动 `ld`**（crt1/crti/crtn 路径发现 + `-lc` + `-dynamic-linker`），编译与链接都不再起 gcc。实证：`/root/pvh-build2/scripts/mod/` 下 8 个宿主目标（`modpost.o`/`file2alias.o`/`mk_elfconfig.o` …）的 `.cmd` 里都是 `dcc-host-wrapper`，`modpost` 二进制已链出；`fixdep` 是**每个 TU 都要跑**的门神，它先打通；仓里另有 Rust 版 `fixdep`（`scripts/build_fixdep_dcc.sh` 安装）可当退路。
2. **[已完成] `.note.GNU-stack`**：dcc 的 ET_REL 现在不发这个节（`driver.rs` 里那句
   `-Wl,-z,noexecstack` 就是在绕它），而 gcc/clang 默认发。补上既去警告也与 gcc 产物同形；
   站点在 `codegen/shared/elf.rs::build_elf_rel_x86_64` 的固定节表**末尾追加**（节号是固定表：
   `.text`=1…`.tbss`=6，自定义节从 7 起，`resolve.rs` 有成片 `7 + sec_idx` 依赖它，
   所以只能追加不能插入）。
3. **[验证中] 16 位代码**：`arch/x86/boot/**`、`arch/x86/realmode/rm/**` 是 `-m16` 的 setup/实模式代码，
   dcc 没有 16 位后端，现在由 `delegate_gcc "m16/m32-no-dcc-backend"` 交 gcc（在新要求下这条路
   必须关）。两个选择：**(A)** 给 dcc 加 `-m16`（指令编码与寻址窗口都要改，成本最大）；
   **(B)** 验收走 **PVH**：`CONFIG_PVH=y` + QEMU `-kernel vmlinux`，启动不经过 setup，
   16 位那批文件根本不进这条链。先走 (B) 打通用户空间，(A) 作为独立里程碑（要 `bzImage` 时才需要）。
4. **[待验] 模块侧**：10568 个 `=m` 让 `modpost`、`.ko` 链接（`$(LD) -r`）从「可选」变成**必经**
   （本配置实测 `CONFIG_MODULE_SIG`/`CONFIG_MODVERSIONS` 均未开，签名与 CRCs 不在链上）；
   `CONFIG_OBJTOOL=y` 同样要求 `tools/objtool/**` 能被 dcc 编（它依赖 libelf 头文件与
   `list.h` 那一族宏）。

## 当前进度（2026-10-28 → 2026-11-03）

**11-03 的状态**：PVH 全树链已经走到 `vmlinux` 前的两个必经阶段（STUBCPY、modpost），各撞一个缺陷，均已修并各带一条集成用例；随后按「符号表修复影响每个 TU」的定义做了一次全量重编（build9，删掉全部 2987 个 `.o`）。三条修复：

| 缺陷 | 现场 | 根因 | 修的位置与实证 |
|------|------|------|----------------|
| `__efiapi` 括号化函数指针声明符（属性写在 `*` **前**） | build6 停在 `drivers/firmware/efi/libstub/alignedmem.o`：35 条 `E0001/E0003/E0009` | 三条声明符路径的属性探测都在 `parse_ptr_suffix` **之前**，基类型自带 `*` 时 cur 还停在 `*` 上，永远看不到那个 `(`；typedef 另有分支先把 `(` 当名字吃掉 | `parser.rs` 新增 `paren_decl_attr_ahead()`，typedef / 字段 / 全局三处都在「基类型的 `*` 已吃完」之后探测。实证：`alignedmem.o` 单目标编过；`/tmp/efi_run.c` 运行时（typedef/字段/全局/调用，返回 42）exit=0；用例 `kernel_paren_funcptr_attr_declarator` |
| EFI stub 拒收绝对重定位 | build7 停在 STUBCPY：`vsprintf.stub.o: absolute symbol references not allowed in the EFI stub`，33 条 `R_X86_64_64` 全在 `.data` | libstub 是唯一带 `-fPIC` 的目录，而 dcc 把 `-fPIC` **直接丢掉**（`"-fpic"` / `"-fPIC"` 那条臂是空块）；稠密 switch 的跳表降级不看 PIC，表项是 `.quad 标号` 的绝对 64 位地址 | `Config::pic` 真接旗标（`-fPIC` 置位、`-fno-pic/-fno-PIE` 归零），`optimize_with_full_target`/`run_ssa_passes` 透传，`unstructured && !pic` 才上表（非 PIC 的内核主体行为不变，增量指纹也计入 `pic`）。实证：重编 `vsprintf.o` 后 `R_X86_64_64` 计数 **0**、`DCC_SWITCHTABLE_DUMP=1` 不再打出 hoist、`make ... vsprintf.stub.o` 通过（11616 B）；用例 `switch_jumptable_gated_by_pic`（非 PIC 必须有表 / `-fPIC` 必须无表，两条互为对照） |
| 导出符号标号不进 symtab | build8 走到 modpost：**12771** 条 `vmlinux: .export_symbol section references 'x', but it does not seem to be an export symbol`（另 7 条 `contains strange symbol`，`.rela.export_symbol` 共 12866 条），随后 SIGSEGV（`vmlinux.symvers` 退出 139） | `include/linux/export.h` 的条目是 `.section ".export_symbol","a"` + `__export_symbol_##sym:` + `.quad sym`：标号**只定义、从不被引用**、也没有 `.globl`。dcc 的建表只有两条路会给标号发符号（`.globl`、被重定位引用），于是 `vmlinux.o` 里 12866 条导出只有 `.S` 汇编文件贡献的 **88** 条有符号 ⇒ modpost 的 `find_fromsym` 退到前一个导出名 | `elf/rel_symbols.rs` 建表前补一趟：数据节里所有「命名标号」按 GAS 口径发 LOCAL/NOTYPE（排除 `.L`/数字/可执行 pushsection），按 (节号, 偏移, 名) 排序保可复现。实证：`lib/string.o` 的 28 条 `__export_symbol_*` 全在（值 0/0x10/… 与 gas 同形）；用例 `asm_label_in_data_section_emitted_to_symtab`。**代价**：符号表修复改变每个 C 对象的 symtab ⇒ 只能全量重编（见下面 build9） |

build9 的现场记录：`/root/pvh-build2/vmlinux-build9.log`（`make O=/root/pvh-build2 -j2 CC=scripts/dcc-kernel-cc.sh HOSTCC=scripts/dcc-host-wrapper.sh HOSTLD=ld vmlinux`）。

换档后的现场：配置由 `KCONFIG=localmodconfig bash scripts/kernel_pvh_verify.sh --config` 落到 `/root/pvh-lmc/.config`（脚本先 `defconfig` 再 `localmodconfig`，再 `scripts/config -e PVH -d SUSPEND -d HIBERNATION -d ACPI_SLEEP -d IA32_EMULATION -d COMPAT` + `olddefconfig`），构建日志 `/root/pvh-lmc/vmlinux-build1.log`。

**`fixdep: error opening file: .../<stem>.o.d` 的定性（两次，均非 dcc 缺陷）**：`--build` 在 `arch/x86/events/intel/knc.o`、`arch/x86/kernel/fpu/core.o` 两处报同一句 `fixdep: error opening file: …/.<stem>.o.d: No such file or directory`，make 随即删掉该 `.o`、rc=2；两次都**无法单点重现**（`make … arch/x86/kernel/fpu/core.o` 编过、`.cmd` 85 KB 带 `deps_`）。判据来自 kbuild 自己的写法：`scripts/Kbuild.include::cmd_and_fixdep` 是一条 shell 串 `$(cmd); fixdep $(depfile) $@ > $(dot-target).cmd; rm -f $(depfile)`，而 `scripts/basic/fixdep.c::main` 先 `printf("savedcmd_%s := …")` 再 `read_file(depfile)`——失败现场 `.core.o.cmd` 恰好只有 `savedcmd_` 没有 `deps_`，说明 dcc 已写完 `.d`（`driver.rs` 在 `run()` 里生成 `.d` 早于 codegen，且日志有 `dcc: wrote …/core.o`），`.d` 是在 fixdep 打开它之前被删掉的。删除窗口就是上面那句 `rm -f $(depfile)`：被 `kill -9` 摘掉的 make 不带走 recipe shell，遗留 shell 跑完自己的 `dcc` 后仍会执行这句 `rm -f`，正好删掉新 run 写出的同名 `.d`。收尾动作：中断构建按**进程组**杀（`kill -9 -- -PGID`）或事后 `pgrep -f target/release/dcc` 确认无残留；遇到该报错只需删掉该 TU 的 `.o` 重跑，不必怀疑依赖文件写入。

**11-03 回归与旁证**：`cargo test --release -j2` 下的 lib **2594 通过 / 2 ignored**、integration **1285 通过 / 6 ignored**、`disasm_snapshot` 33 例通过（`pr105613` 重录：当前 codegen 的帧比 10-26 那版多 16 B（0x300 → 0x310），指令逐条相同、仅位移不同，同一产物跑 dg-do-run 主体 exit=0）。另外两条真树旁证：① 所有已重编对象的 `.export_symbol` 重定位都能按 modpost 的 `find_fromsym` 口径找到同名 `__export_symbol_*`（479 个对象 / 4334 条，0 例外；对 `ld -r` 合并后的 `drivers/gpu/drm/ttm` 与 `net/ethtool` 归档对象再查也全对得上，82 + 48 条）；② `tools/objtool/objtool` 本体也是 dcc 编 + `ld` 链（1.7 MB），能正常解析 dcc 对象并生成 `.orc_unwind`，但对 dcc 的 `mov %r13,%rsp` 收尾不认（objtool 的 CFI 跟踪只特判 `rbp`，`check.c` 里 `CFI_BP` 分支之外没有帧寄存器路径），因此每个带帧的函数都报 `return with modified stack frame`（build9 累计 **46754** 条，全是 warning、零错误）。影响面限于 epilogue 处的 ORC 条目（返回地址落在 epilogue 的帧才会算错 CFA），不阻断启动，已记为独立待办。③ `scripts/kernel_pvh_verify.sh --partial` **14/14** 通过：六个 `-m16` 的 `.S`（`header`/`trampoline_64`/`stack`/`reboot`/`bioscall`/`copy`）由 dcc 编成 ELF32 且与 `gcc -m16` 产物（剥调试后）逐字节相同，`-m16` 的 C 输入仍如实报 `DCC-GAP + E0319`、陷阱 0 次。注意脚本原按**源码树**找 `generated/`，在 `O=` 构建下必失败；已改为 `$BUILD/…generated` 并按 kbuild 的 include 顺序排列。

**11-02 的状态**：全树第一阻塞（jump-label / KCOV 否决 `__always_inline` 展开）已清；预处理成本从 **143 s/TU 降到 6.9 s/TU**；本轮又补了三条真树硬错（SIB 寻址、asm 助记符大小写、SGX 的 ENCLS/ENCLV/ENCLU），`arch/x86/kernel/cpu/{resctrl/pseudo_lock,sgx/encl}.o` 已能编出（172,648 B / 264,048 B，`encl.o` 里 6 条 `encls`）。剩下的卡点是**前端内存悬崖**（`net/core/skmsg.c` 单 TU 吃 6.4 GB 被 OOM 杀，见下面那行）与 **codegen 耗时**。
在跑的 `make -k -j2 vmlinux` 用只读快照 `.work_tmp/dcc-tree`（依次 md5 `5aa35f71…` → `16a183c0…` → `bed3964d…` → **`3c4bd955…`**），日志 `/root/kwlogs/allmod-5.log`（330 个 `CC`）与后续 `allmod-6.log`。换快照前后在树速率：0.83 → **1.8 个对象/分钟**（同一 `-j2`）。回归：lib 2589 / integration 1267 / `check_opt_equivalence.sh` 0 OPT-DIFF，全绿。

**磁盘与速率（11-02 05:40）**：`~/.dcc/cache` 长到 21.2 GB（其中绝大部分是本轮量测用
`-DSIZEPROBE/-DPERFCOLD/...` 盐出来的死键），用 `scripts/cleanup_disk.sh --only 1` 清掉后磁盘从
49 GB 空闲回到 **71 GB 空闲**。该脚本以前只有「默认清 1-6」与「--all」两档，而默认档会连
项 6（`.work_tmp/`）一起清 —— 那里现在放着文档里引用的复现夹具与量测装置，所以加了
`--only <项号>` 选择器（项号→函数的表只一份）。换快照前后的在树速率对比（同一台机器、
同一 `-j2`）：02:57→04:23 是 0.83 个对象/分钟（111 个 / 133 分钟），05:25→05:40 是
**1.8 个/分钟**（27 个 / 15 分钟，其间 05:37 还清了一次缓存）——预处理那两件共给了 ≈2 倍；
剩下的天花板已量到具体环节（见下面「速度的下一块石头」）。

本轮把全树干净基线跑到 **383 个 .o**，收集到 5 个失败 TU（`arch/x86/kernel/fpu/xstate.o`、
`arch/x86/events/core.o`、`arch/x86/kernel/ldt.o`、`arch/x86/kernel/cpu/bugs.o`、
`mm/mm_init.o`），定位到两类根因并修完；接着拿修好的二进制把 `ld -r` + objtool 预跑了一递，
又揪出一个链接阶段才暴露的缺陷（`.section`/`.previous` 退不回原节 → objtool
`bad .discard.annotate_insn entry`）。另外发现强制内联的新形状与 `__builtin_constant_p`
死分支互撞会把本来能编的 TU 顶坏，已按名退回并记账。

**工作方式（本轮按用户要求改定）**：不再每轮 `make clean` 从 0 重编。上一轮已产出的 `.o`
留着，改完 dcc 后直接 `make -k` 续跑，Kbuild 只重做失败与缺失的目标（依赖 `.cmd` 指纹，
换了 CC 命令行会自然重编那一步）；全树都跑通到 `bzImage` + QEMU + LTP 之后，再做**一次**
从 0 的干净重编作为可复现验证。本轮的 full6 就是这样续跑的（`LOG=/root/kwlogs/full6.log`）。

| 缺陷 | 症状 | 根因 | 修的位置 |
|------|------|------|----------|
| 强制内联不下探**循环条件** | `error[E0311]: ... (expression: bit, in function _static_cpu_has)` | `opt.rs::inline_calls_in_stmt` 的 `Stmt::While`/`Stmt::Do` 两支只往下走 body，不看 cond（`For` 支一直有）；调用点残留 ⇒ 编译器另发 callee 副本，而带 `"i"(形参)` 的副本编不出来 | 两支各补一条 `inline_expr(cond)`，与 `For` 同口径；因下行冲突，现由 `DCC_INLINE_LOOP_COND` 管且默认关 |
| 强制内联不下探**语句表达式** | 同上，站点在 `WARN*()`/`__clear_user()` 那一族宏里 | `inline_expr` 没有 `Expr::StmtExpr` 这一臂，落进 `other => other`，`({...})` 里的调用点没人看过 | 新增一臂，块体走与函数体同一套 `inline_calls_in_items` |
| 强制内联不下探**实参里的调用** | `f(g(...))` 里 `g` 展开了、外层 `f` 仍留副本 | `inline_forced_call_as_stmt_expr` 以「实参有副作用」否决；而那个副作用正是实参里待展开的调用 | 实参下探后重试一轮（`try_inline_call_at_expr_pos`），重试仅在「否决来自我们自己的展开」时放宽该判据 |
| 内联 asm 体内的 `call` 毁掉延伸池家 | `error[E1136]: call 到 'write_ibpb' 不经过 caller-saved 家的保存窗口` | 内核跳进另一个 C 函数的 asm 只写 `ASM_CALL_CONSTRAINT`（里面没有寄存器号），而按 ABI 这次调用毁掉的是全部 caller-saved GP；原有守护只看用户写的 clobber 名单 | `asm/emit.rs`：体内检出 `call` 就把 SysV caller-saved GP 并进守护名单，走既有 push/pop 区间；家全都有着落时开一层 `asm_call_window`，让 `call_label` 的 E1136 守卫不再顶住编译 |
| `.section X,"a"` + `.previous` 退不回原节 | 链接前 `ld -r` + `objtool --link --ibt --noabs` 报 `bad .discard.annotate_insn entry: 0 of type 8`（单看编译全绿） | `.section` 把「即将进入的节」同时压进两条栈，`.previous` 弹出后又原下标压回 ⇒ `pushsection_stack` 永远留着那个已退出的数据节，之后每条 asm 的数字标号都按它的坐标登记（实测 `mm/mm_init.o` 的 5 个 `ANNOTATE_REACHABLE(1b)` 标号全在 `.export_symbol`+0x80） | `asm/emit_lines.rs`：`.section` 存「切换前那个节」（栈空用 `usize::MAX` 哨兵），`.previous` 按它恢复 |
| 循环条件展开与 `__builtin_constant_p` 死分支互撞 | 把 `while`/`do-while` 的条件送进内联器后，`kernel/sched/build_utility.o` 从能编变成报 `E0311: ... (expression: (1 << (__inl$_nr & 7)), in function __wait_on_bit)` | 看起来像「循环条件不该下探」，实际是两个独立缺口叠在一起：条件里的 `__builtin_constant_p(形参)` 三元**根本没被折**（见下面那行），于是两条分支都被展开；死分支带进 `Stmt::Label` 后 `fold_branches_block` 以「goto 可跳入死分支标签」为由拒剪 | ✅ **已修**（2026-10-28：两道一起修 —— `fold_expr` 的 `Expr::Cond` 臂改走 `const_eval` 整体求值、`subst_cand_reads_items` 的候选表接「链」，另加 `prune_spliced_items`（形参临时量代回 + 趁早折分支，放在递归展开之前）作为同一条拦网。`DCC_INLINE_LOOP_COND` 因此翻回**默认开**；`wait_bit.o`、`ptrace.o`、`build_utility.o` 在开着时实测全部能编，用例 `forced_inline_in_loop_conditions_expands` 钉住行为。留着闸门是给配对 A/B 与逐形退回用） |
| 端口 I/O 的 `%b/%w/%k/%q` 只认寄存器 | `error[E0311]: unsupported in port operand None` / `unsupported out port operand None`，站点 `arch/x86/include/asm/shared/io.h:22`（`inb %w1, %b0` / `outb %b0, %w1`，`"Nd"` 把端口折成立即数） | `asm_reg_modifier_subst` 不分操作数类型先 `parse_asm_reg(val)?` 直接退出 ⇒ 非寄存器操作数拿不到代回，`%w1` 原样留在模板文本里，操作数解析成 `None` | ✅ **已修**（2026-10-28：GAS 对立即数/符号/内存操作数**不改宽度**（宽度由助记符后缀承担），只有 `%hN` 在寄存器之外无意义；非寄存器操作数原样代回。`constraint.rs` 里那份逐字相同的副本改成转发，只一份实现。诊断也补上被拒操作数的形状（以前只说 unsupported，只能猜）。产物与 gcc 逐字节同：`out %al,$0x21`=`E6 21`、`in $0x21,%al`=`E4 21`、`in $0x43,%ax`=`66 E5 43`。用例 `port_io_immediate_operand_keeps_immediate_text`） |
| 表项重定位指进 NOP 填充区（initcall 一类的函数指针表） | `objtool` 在合并对象上报 `data relocation to !ENDBR: .text+0x…`，而那个地址 `objdump` 看是一串 `nop`；`arch/x86/events/amd/ibs.o` 的 symtab 里 `amd_ibs_init` 有**两条**（`FUNC@.init.text+0x900` 与 `NOTYPE@.text+0xcbd0` size=0），`.rela.initcall6.init` 的符号号指向后者 | `elf/rel_symbols.rs` 里 `pending_custom_sec_rels` 那趟补符号用 `self.sym_index` 当「已登记」去重门，而 `sym_index` 要到函数末尾「构建最终符号表」才 `clear()` 重建 ⇒ 门等于没门；同名两条在末尾建表时 `insert` **后者覆盖前者**，凡按名字发重定位的地方全拿搬走前的 `.text` 位置。同文件另两路（`pending_code_rels`/`pending_data64_rels`）用的已是 `known_syms` —— 又一个「同一规则两份实现」 | ✅ **已修**（2026-10-28：那一趟改用 `known_syms.contains` 并在登记后 `insert`，与另两路同口径。实测回放 ibs.o：只剩一条 `FUNC@.init.text+0x900`，`.rela.initcall6.init` 的 `Symbol's Value` 变 `0x900`，与 gcc 同构（gcc 发 `.init.text + 10`）。影响面用 `.work_tmp/kwduppurge.sh` 扫「同名 FUNC+NOTYPE 且节号/偏移不同」：命中 209/1037，只删这些重编（其余产物字节不变） |
| UND 符号发 `STT_FUNC` 而不是 `STT_NOTYPE` | `make vmlinux.o` 以 **错误 255** 失败；objtool 只报一行致命：`retbleed_return_thunk: can't find starting instruction`（日志里 26524 行 objtool 输出其余全是警告） | GAS 对未定义引用**从不**标函数类型（实测 gcc 的 `arch/x86/kernel/nmi.o`：24 条 UND 全 NOTYPE，含真函数 `strcmp`/`synchronize_rcu`），dcc 同对象发 4394 条 UND FUNC。`arch/x86/lib/retpoline.S` 用 `SYM_CODE_START*` 把 `retbleed_return_thunk` 定义为 NOTYPE，那个地址按设计不是指令边界；`ld -r` 会把引用侧的 FUNC 带到这条定义上，而 objtool 的 `create_prefix_symbol()` 开头 `if (!is_func_sym(func)) return 0;` 只对 FUNC 去 `find_insn()` ⇒ NULL ⇒ `return -1` ⇒ 255。同一个规则在 `rel_symbols.rs` 里有 6 处入口，其中两路已发 NOTYPE、四路发 FUNC | ✅ **已修**（2026-10-28：6 处 `SHN_UNDEF` 发射全部改 `STT_NOTYPE`。回放 `nmi.o`：UND 变 `4405 NOTYPE + 700 OBJECT`，与 gcc 同形。这条触及每个有外部引用的对象 ⇒ 只能按 §5.0 标准判据清全树重跑（`full11`）。用例 `undefined_symbol_refs_are_notype_like_gas` 钉住「一条 UND FUNC 都不许有」+「本 TU 定义的全局函数仍必须是 FUNC」的反向护栏；待全树跑完后随 `cargo test` 一起实跑） |
| 常量 `switch` 不折 ⇒ 「编译期尺寸检查」陷阱函数被引用 | `vmlinux` 最终链接报 1905 行未定义引用，顶部是 `__xadd_wrong_size` 269、`__bad_size_call_parameter` 261、`__cmpxchg_wrong_size` 242、`__xchg_wrong_size` 53、`__get_user_bad`/`__put_user_bad` 各 ~50 —— 这些函数**故意不存在**（`#define __raw_cmpxchg(…, size, …) ({ switch (size) { … default: __cmpxchg_wrong_size(); } … })`，调用点传常量 `sizeof(*(ptr))`），gcc 折完就不引用 | `opt.rs::fold_branches_stmt` 根本没有 `Stmt::Switch` 臂（只有 `If/While/Block/For`），也不下探 `Stmt::Expr` 里的 `({...})` 与 `Stmt::Do` 的体 ⇒ 这类 switch 从来没人看过一眼 | ✅ **已修**（2026-10-28：新增 `Stmt::Switch` 常量折叠 + `fold_branches_in_expr`（臂表抄 `rename_idents_in_expr` 同形状）+ `Stmt::Expr`/`BlockItem::Decl(Var)` 两条下探入口 + `Stmt::Do` 臂 + `stmt_stops_control`。两个保守前提：只剩「确定不 fall-through」与「被丢臂里没标签」时才折。实测 `tty_ldsem.o` 陷阱真引用 13→5、`.text` 34862→32030；`/tmp/kwsw/s3.c`+`s5.c` 与 gcc 运行期逐字一致。用例 `const_switch_and_do_while_dead_branch_leave_no_trap_references`、`const_switch_fold_preserves_semantics_vs_gcc`） |
| asm 数据表达式里的 `sym - N - .` 链被整串当符号名 | `ld: vmlinux.o:(runtime_ptr_USER_PTR_MAX+0x0): undefined reference to '1b - 8'` | `arch/x86/include/asm/runtime-const.h` 发 `.long 1b - 8 - .`；`parse_asm_data_expr` 只拆**一个**二元运算符，在最后一个 `-` 处切成 `1b - 8` 与 `.` ⇒ 整串成了符号名 | ✅ **已修**（2026-10-28：`constraint.rs` 新增 `split_relative_terms`：要求恰好一个非数值非 `.` 项、其余全是常数或 `.`，常数带符号累加成 addend。实测 `fs/namei.o` 的 `.relaruntime_ptr___names_cache` 变成 `R_X86_64_PC32 .Lasm_local_313_1_0 - 8`，与 gcc 同构） |
| `__builtin_constant_p(内联临时量)` 作三元条件时不折 | `impossible constraint in 'asm' … (1 << (__inl$_nr & 7))`，实测 `kernel/ptrace.o` 的 `wait_on_bit`（`include/linux/wait_bit.h:72`） | parser 与 `fold_expr` 的 `Call` 臂对这种调用采取「不急切折 0」（机会留给 SCCP/modref），条件永远不是 `Int`；而 `fold_expr` 的 `Expr::Cond` 臂以前只认 `Expr::Int` ⇒ 三元留在运行时，**两个分支都被编**，非常量的那一支里 `"i"(形参)` 编不出 | ✅ **已修**（2026-10-28：条件先认 `Int`，再用 `const_eval::eval_full` 整体求值——与 `fold_branches_block` 折 `Stmt::If` 同一条判据，`__builtin_constant_p` 的答案也由 `const_eval` 在同一个地方给。另：`subst_cand_reads_items` 的候选表多接一条「链」（初值整体是一个已知候选的读时接过它的值），与上面的折条件合起来才覆盖 `wait_on_bit` 这一形。用例 `builtin_constant_p_ternary_condition_is_decided_at_compile_time`） |
| `sym±N` 当成一个符号名（重定位发射侧） | `ld: main.o:(.altinstr_aux+0x2): undefined reference to 'boot_cpu_data+100'`，全树 417 处 | 内联 asm 的内存操作数文本与 `.pushsection` 数据表达式把整串当名字交下来；早期只在 `pending_data_rels` + 自定义节两条出口上剥过位移，而**真正的量大处**在 `pending_code_rels`（`.init.text` 里 `movl boot_cpu_data+114(%rip)`）与建表侧两条 `implicit_und` 循环上（链接错误报的就是建表登记的那条 UND） | ✅ **已修**（2026-10-29：剥位移收进唯一解析路径 `reloc_sym_index`，返回类型改成 `Option<(u32, i64)>` 让**编译器强制每个出口表态**；建表侧三处用同一个 `split_sym_offset` 归一化，symtab 只留基名。回放 `setup.o`：`.rela.altinstr_aux` 变 `boot_cpu_data + 0x60`、`.rela.init.text` 变 `+ 0x6e`，与 gcc 同形。用例 `asm_symbol_offset_does_not_become_a_symbol_name` + 运行级 `..._matches_gcc_at_all_levels`（夹具同时盖两条出口，并锁住「位移必须真的生效」）） |
| 同名两条 `FuncDecl` 让 DCE 误删被取地址的 `static inline` | `ld: undefined reference to 'btf_id_cmp_func'`，全树 231 处；引用点在一条**没人调用**的 `static inline` 体里 | `include/linux/btf.h:514` 定义 `static inline bool btf_id_set_contains()`（体里取 `btf_id_cmp_func` 的地址），`include/linux/bpf.h:3873` 又有一条**同名非 static 声明**——gcc 接受这个方向。于是声明那条（`body=None`、`is_extern` ⇒ DCE 硬根）与定义那条同名，而 `cross_function_dce_with_roots` 按名字 `insert` 边表 ⇒ **空边表盖掉定义的边**，被引用者判不可达删除、函数体与 `lea` 重定位照发 | ✅ **已修**（2026-10-29：边表改 `entry().or_default().extend()` 合并，「可达集合的闭包」这条不变式才真成立。回放 `timer.o`：`btf_id_cmp_func` 由 `UND NOTYPE` 变 `WEAK FUNC @0x22c0 size=184`。用例 `extern_decl_after_inline_def_keeps_addr_taken_static_defined` + 运行级配对；夹具已用**临时回退**验证能红（回退后 `-O2` 立出 `UND NOTYPE cmp_fn`）） |
| 【操作事故】purge 脚本缺省指向 gcc 参照树 | 一次缺省调用删掉参照树 1101 个 `.o`（输出里那行「gcc 回退产物: 1061」就是现场证据；dcc 树该数字应为 3） | `scripts/kernel_purge_stale_objects.sh` 的缺省 `KERNEL_DIR` 写的是 `$HOME/linux-7.1`（参照树），而脚本语义只对 dcc 树成立；两边 `.o` 外观同构，事后按产物看不出 | ✅ **已修+已恢复**（2026-10-29：缺省改 `$HOME/linux-7.1-dcc`，并加二进制级守卫——树里必须有写 `dcc-kernel-cc.sh` 的 `.cmd` 指纹，否则拒执（全新树无指纹可查时用 `DCC_PURGE_ALLOW_NO_DCC_FINGERPRINT=1` 显式放行）；参照树已 `make -k -j4` 重建回 1141 个 `.o` + `vmlinux` + `bzImage`） |
| 块作用域函数原型把整函数顶出 SSA 通道 | `ld: undefined reference to '__compiletime_assert_NNN'`，全树 181 处（同惯用法 `__scoped_seqlock_*`/`__BUILD_BUG_*` 若干） | `opt.rs::items_have_unsafe_ssa_pattern` 里 `BlockItem::Decl(Decl::Func(_)) => true`。该判据本意是**有体的嵌套函数**（`rebuild_block` 只重建 `Decl::Var`），但块作用域**原型**不产生任何代码：UND 靠重定位名字登记，`__compiletime_error`/`error()` 诊断扫描只看 `prog.decls` 顶层⇒ 拦下零收益。内核陷阱宏体里正好带一条这种原型 ⇒ 整函数进不了 SCCP，`if (常量局部)` 连调用一起发 | ✅ **已修**（2026-10-29：改成 `f.body.is_some()`。旧/新二进制逐形对拍：块内原型那一形从 ✗ 变 ✔；语句表达式初值那一形修后**仍未折**（另一条 bail），已当已知缺口写进夹具注释不进断言。用例 `compiletime_assert_block_scope_proto_calls_leave_no_relocation` + 四档对拍；判据只数 `.rela.text`（`elf64_all_rela` 在 ET_REL 上永远空集），另带「活臂引用不得被过修抹掉」反向护栏） |
| 空 `.tdata`/`.tbss` 带 `SHF_ALLOC` | `ld: .tmp_vmlinux1: section '.tdata' can't be allocated in segment 1 LOAD: .data … .modinfo .tdata .tbss`（未定义引用清完后**仍然挡着最终链接**的独立一道），外加 6 条 orphan 警告 | GAS 无 TLS 内容时不建该节（实测 gcc 的 `init/version-timestamp.o` 里没有 `.tdata`/`.tbss`），而 dcc 的 ET_REL 节号是**固定表**（`.text`=1…`.tbss`=6，自定义节从 7 起，`resolve.rs` 成片 `7 + sec_idx` 依赖它）⇒ 不能靠少发一条表项对齐 | ✅ **已修**（2026-10-29：保留表项、空节去掉 ALLOC/TLS 标志；非 ALLOC 节不进 ld 的段分配。实测新二进制 `[5] .tdata`/`[6] .tbss` flags 为空。旧用例「空 .tdata 也必须带 SHF_TLS」是被测错了方向，已改成有内容才带并写明证据） |
| asm 位移写成四则式被当符号名 | `ld: undefined reference to '0*8'`（全树 7 处，站点在 `csum_partial` 附近、间隔 7 字节），**且寻址本来就是错的** | `arch/x86/lib/csum-copy_64.S:41` 的 `movq %rbx, 0*8(%rsp)`：`parse_asm_mem` 先拿第一个 `(` 切分，再用 `parse::<i32>()` 试位移，失败就整串当 `SymDisp` 符号名 ⇒ RIP-relative + 丢掉 `%rsp` | ✅ **已修**（2026-10-29：新增 `operand.rs::fold_asm_disp_number` 作唯一判据（与 `$N` 同用的 `eval_pure_numeric_arith`），`operand.rs`/`constraint.rs` 两份逐字相同的 `parse_asm_mem` 共用它；lib 用例正反两面（`0*8/3*8/5*8(%rsp)` 必为 `Mem{disp,base=rsp}`；`foo*8`/空串/溢出 i32 不得当数值折）。过程中另记一条切分限制：`(4+1)*8(%rsp)` 会被第一个 `(` 误切成寄存器组开头，已写进用例注释） |
| 文件级 asm 里 `.global` + 数据节标号不进 symtab | `ld: undefined reference to 'skl_call_thunk_template'`（4 处）/ `'int3_selftest_asm'`（3 处） | 两层：① `.global` 在**数据 pushsection** 那条路（`elf/output.rs::emit_pushsection_data`）与 `asm/directive_emit.rs` 里都被当「已消费的不发射指令」吞掉，名字从来不进 `global_asm_exports`；② 就算定义了发出来，`extern unsigned char tmpl[];` 这类无定义声明另走一路留一条 UND，而建表时 `sym_index.insert` **后写覆盖先写** ⇒ 重定位全绑到 UND | ✅ **已修**（2026-10-29：两处各补登记；定义路先查 `pushsection_label_offsets`（数据节真坐标）再查 `labels`（只是 `.text` 坐标，先查会把 `.rodata` 模板发成 `FUNC@.text` 的**静默错地址**）；建表前加一条通用规则「本 TU 已有定义的名字不得再留 UND」。实测 `callthunks.o`/`alternative.o` 回放 rc=0，`skl_call_thunk_template/_tail` 与 gcc 同为 `NOTYPE GLOBAL`。**遗留**：`_tail` 的节内偏移也是 0（gcc 是 8）⇒ `SKL_TMPL_SIZE` 算成 0，运行期拿不到模板长度，已开待办） |
| E1128：帧预算两侧用不同尺 | `error[E1128]: frame budget exhausted in 'blk_mq_extract_queue_requests' while allocating '<sret>'`，整 TU 硬错（实测全树 `block/blk-mq.o`） | 发射侧为「目标地址是运行时地址的聚合赋值」多占一个 `alloc_sret_slot(8)`（`AggDst::Indirect` 那形），而规划侧 `sret_bytes_expr` 的 `Expr::Assign` 臂只算两个子表达式（都不是 Call）⇒ 登记 `sret_need=0`；补上后又红一次：登记的是**原始 8 字节**而分配器按 16B 步进 | ✅ **已修**（2026-10-29：发射侧形状判据抽成共用的 `assign_needs_addr_slot`；抽出 `Codegen::sret_slot_bytes()` 当唯一那把尺，`alloc_sret_slot` 与扫描的 Call/VaArg/Assign 臂全改用它。实测 `block/blk-mq.o` 回放 rc=0。另永久留下了诊断：`check_frame_budget` 现在打带坐标 + `#[track_caller]` 的发射站点日志（不改产物字节），本轮就是靠它一眼认出 `dispatch.rs:1134`。用例 `aggregate_assign_through_runtime_address_is_budgeted`） |
| `(void)(x)` 丢弃值的纯读照样发码 | `ld: undefined reference to 'RCU'`（实测 `io_uring/tw.o` 的 `io_ctx_mark_taskrun()`），另有同一族的 `RCU_BH`/`RCU_SCHED` 等形状 | 内核 `token_context_lock(name,…)`（`compiler-context-analysis.h:242`）只发 `extern const struct __ctx_lock_##name *name;`，注释明写「not backed by a real data structure (linker error if accidentally referenced)」——这类名字**天生没有定义**；而 `rcupdate.h:456` 把 `lockdep_assert_in_rcu_read_lock()` 定成 `do { (void)(RCU); } while (0)`。`Expr::Cast` 的 void 臂以前只筛字面量，`Ident` 依旧发码 ⇒ 值虽丢、**重定位留下**（gcc 一个字节都不发） | ✅ **已修**（2026-10-30：新增语义层唯一判据 `discarded_value_needs_code`（黑名单式：只有结构上碰不到世界的形状才返 false；`StmtExpr`/`AsmExpr`/`VaArg`/会陷入的除模一律留着），接在 **AST 层** `opt.rs::prune_dead_stores` 新增的 `discarded_statement_is_dead` 上（两道闸：结构 + 按声明的可观察名字集），并把「这个声明每次访问是否可观察」抽成 `ast::var_decl_access_is_observable`（原 `codegen::frame_access_is_volatile` 与 `ast_gvn::volatile_globals` 各写一份，现都走它）。**先试过发射器侧（`cast.rs` 的 void 臂）后退回**：发射器拿不到 `VarDecl::quals`，把 `(void)(vglob)` 的 volatile 读误删（实测引用 1→0）。判据因此跑 -O2/-O3（`prune_dead_stores` 是 `level >= 2`）。夹具 `token_ctx_lock_discarded_read.c` 把「该删的」与 call/volatile 读/真取地址三条「绝不能删的」钉在同一份产物上，用例 `discarded_pure_read_leaves_no_reference_while_effectful_arms_do`。实测：未修二进制 -O0 有 2 条 `R_X86_64_PC32 RCU` + `UND RCU OBJECT GLOBAL`；新二进制回放 `io_uring/tw.o` rc=0 且 RCU/RCU_BH/RCU_SCHED 三条重定位与 UND 全归零 |
| `__builtin_types_compatible_p` 把不兼容的类型判成兼容 | 不是「少一个优化」而是**走错臂**：现场产物级症状是链接期 `undefined reference to '__compiletime_assert_NNN'`（实测 `mm/shmem.o` 的 408/410 两条，条件就是 `if (!(!cpt)) trap()`，cpt 应为 0） | `parser.rs::types_compatible` 里有三条与 gcc 实测相反的宽松规则：① 「`void *` 与任意对象指针兼容」（注释说给 `_Generic` 用，但三个调用点共用这一份，`_Generic` 也不该有这条）；② 同宽度的有符号/无符号整型归一组（`int`/`unsigned`、`short`/`ushort`、`long`/`ulong`），还把 `char`/`signed char`/`unsigned char` 并成一组（C 里是三个不同类型）；③ 数组只比元素基类型、不比元素限定符 | ✅ **已修三条、余两条已知缺口**（2026-10-30：改成严格同变体 + 无 void 捷径 + 比数组元素 quals）。判据表用「数组尺寸编码」读值（`char pXX[1 + 2*cpt]` 后 `nm -S` 读 size，避开「static 函数被内联进 main」的反汇编归属陷阱）逐形对 gcc -O2：25 形里 23 形已一致。用例 `types_compatible_p_table_matches_gcc`（默认 28 形全绿）+ `types_compatible_p_known_gaps`（`#[ignore]`：`char` vs `signed char`、`int (*)[4]` vs `const int (*)[4]`）。两条缺口根因：`signed char` 在声明路径上归 `Type::SChar`（`parser.rs:7660`），而内置的类型参数走 `parse_type()` 另一条路 ⇒ 同一件事两个出处。实测效果：`mm/shmem.o` 陷阱重定位 9→7、`mm/memory.o` 9→6、`mm/rmap.o` 7→6 |
| `__builtin_constant_p(<写一次的常量局部>)` 返 0，而 gcc -O1 以上返 1 | 内核 `BUILD_BUG_ON`/`compiletime_assert`/`statically_true` 那一族的**可达性全押在这个内置上**：返错值就是把死臂留在产物里 ⇒ 一堆 `undefined reference to '__compiletime_assert_NNN'`（全树最多的一族） | `ipa_modref.rs::proven_const_locals` 把「必须被取址且取用点全安全」当唯一口径，于是 `int x = 7;`（**根本没被取址**）反而不入候选表；另一面是 `"m"`/`"=m"` 内存约束把对象**地址**交给未知代码而 AST 上没有 `&`，`addr_of_counts` 看不见它 | ✅ **已修**（2026-10-30：新增「隔离」口径（从不被取址 + 从不被写 + 不在任何 asm 操作数里）；`BodyFacts` 新增 `asm_named`，由 `collect_asm_ident_names` 递归收 asm 操作数子树里**所有**具名对象，`Expr::AsmExpr` 与 `Stmt::AsmGoto` 两处都记。形状表 `.work_tmp/constp_shapes.c` 逐形对 gcc：`int x=7`、`int x=(3+4)*2` 两形已从 gcc=1/dcc=0 变成一致；`int a=2; int b=a*3;`（初值需要链式前递）与 `struct S s={5}` 的成员读仍差，已开待办。本轮改动 lib 2585 全绿，无回归） |
| `__builtin_constant_p(<内联形参>)` 在「声明初值」这个调用点恒答 0 | 内联落在 `p = f(字面量)` 这一位置时，callee 的体被包成 `({ … __inl$_x = 字面量; … })`，形参临时量声明在**语句表达式**里；`ipa_modref::collect_vars_in_block` 只收 `Decl::Var` 本身、不下探 `v.init` ⇒ 候选表里根本没有那个临时量 ⇒ constp 恒 0 ⇒ `BUILD_BUG_ON` 的断言调用留下（实测 `fs/xattr.o`/`fs/pidfs.o`/`kernel/events/hw_breakpoint.o` 各 2–4 条 `__compiletime_assert_473/487`；体内带 `restart:` 标号的 `__rhashtable_lookup` 就是这一族，`cprop` 那条「嵌套块见标号就整块拒收」的筛子让它没别处可救） | ✅ **已修**（2026-10-31：`collect_vars_in_block`/`collect_vars_in_items` 补「声明的初值也走 `collect_vars_in_expr`」。逐形夹具 `.work_tmp/cp1..cp12` 对 gcc 全部一致，8 形 UND 为空；现场 8 个 TU 用 `kwcheck10.sh` 重放全部 `traps=0 extra=0`。用例 `builtin_constant_p_param_across_decl_init_inline_is_one`） |
| 内置链接器的静态 ET_EXEC 「链成功」但产物一发就段错 | `-fbuiltin-linker` 链一个 `target_clones` + `printf` 的夹具：rc=0、`file` 说 statically linked、`./a.out` 直接 SIGSEGV（rc=139） | 「允许未定义」名单有**两个入口**，§3.26 只堵了 `resolve.rs` 里那个隐式名单：`driver.rs` 里还有一张硬编码表（`printf`/`malloc`/`free`/`memcpy`/`_start`/`main`…）与 P9 那段「把宿主 libc 导出符号全扫进 `allowed_undef`」。而 `link/mod.rs:428` 定死 `emit_dynamic = matches!(output_kind, PIE | Shared)` ⇒ `OutputKind::Executable` 永远是静态 ET_EXEC（无 PT_INTERP/DT_NEEDED），这些名字运行时没人满 | ✅ **已修**（2026-10-30：`Executable` 下清空硬编码表、libc 自动扫描只对 `PIE` 跑。实测现在 rc=1，先报「unresolved symbol(s) came from the BUILT-IN linker path … no PT_INTERP … Drop -fbuiltin-linker to link via the host cc」再 `error[E0302]: symbol 'printf': undefined reference`，不产出坏二进制。没有任何用例依赖旧的静默绑 0 行为：整套 `lib 2585 / integration 1247` 全绿。底线：**链上 ⇒ 产出必须能跑**，这条规则不允许被「允许未定义」名单绕过） |
| `sizeof`/`_Alignof` 的结果类型是 `int` | 连带把 `__auto_type b = sizeof(x) - 1;` 与 `typeof(sizeof(x) - 1)` 推成**有符号 int**，于是内核 `min()/max()` 的符号性检查选错臂 ⇒ 「故意不存在」的 `__compiletime_assert_NNN` 留在产物里 ⇒ 链接期 `undefined reference`（实测 `lib/kstrtox.o` 一个 TU 11 条） | `ast.rs` 里 `Expr::ty()` 对 `SizeofTy/SizeofExpr/AlignofTy/AlignofExpr/Offsetof` 写死 `Type::Int`（C11 6.5.3.4p4：结果类型是 `size_t`，LP64 上 = `unsigned long`），而 `typeof(表达式)`/`__auto_type` 的推导直接吃这个值 | ✅ **已修**（2026-10-30：改成 `Type::ULong`，`Offsetof` 跟着 parser 写的 `result_ty`；同时改对 `parser.rs::collect_upvars_expr` 里那句已过时的注释。定位路径很短：五秒夹具 `min(count, sizeof(buf)-1)` 复现 → 四形矩阵（只换临时量声明写法）定位到「从含 sizeof 的表达式推类型」→ 把「有符号?10:0+sizeof」编成返回值的六形探针读数（gcc=8、dcc=14）。新判据：`sizeof_result_type_is_size_t_matches_gcc`（运行级，含一条有符号 int 反向对照）+ `min_signedness_assert_with_auto_type_leaves_no_trap_reloc`（产物级四形）。实测 `lib/kstrtox.o` 11→0，整套 `lib 2585 / integration 1249` 仍全绿）|
| `__builtin_strcmp(字面量, 字面量)` 不折成常量 | 内核 `kernel/sched/fair.c:537-554` 的 `vruntime_cmp(A, "<", B)` 把「选哪个运算符」编成字符串参数，整套结构是 `if (!__builtin_strcmp(CMP_STR,"<")) … else __BUILD_BUG_vruntime_cmp();`（注释原话 “throws linker error”）⇒ 不折那次 strcmp 就留下永不执行的 else 臂里的引用（实测 `kernel/sched/fair.o` 7 条，分属 7 个展开点）| `__builtin_strcmp` 在 dcc 里只出现在 codegen 的兼容表（`special/builtins.rs`），`const_eval` 完全不认它 | ✅ **已修**（2026-10-30：`const_eval.rs::eval_intrinsic`（常量内建求值的唯一入口）新增 `__builtin_strcmp`/`__builtin_strncmp(n 为常量)` 两条，仅当两边都是字符串字面量（可裹转型）才折；助手 `literal_string_bytes` + `c_strcmp`。判据夹具 `fixtures/c/vruntime_strcmp_trap.c`：产物级零陷阱引用（-O2/-O3）+ 运行级 12 形（-O0/-O2/-O3）。**注意一个无定义点**：C 只规定 `strcmp` 返回值的符号不规定大小（gcc 常量折叠给 ±1、glibc 运行时给 ±99），所以运行级只断言 `SIGN()` 三态，并先拿 gcc 确认两个档位都是 0）|
| 常量桩代回的**调度次序**错：排在 `function_inline` 之后 ⇒ `static inline` 桩一条都消不掉 | 全树剩下 ≈129 行未定义引用里已知的最大一块：`fs_bio_integrity_*`(14) + `__blk_crypto_*`/`__bio_crypt_*`(14) + `efi_*`(11) + `__pi_`(10) + THP/dax/cma/xbc/free_zone_device 那 ≈30 行。同一个 `map_kernel.o`：gcc 参照树 0 条 `__start_bss_decrypted` UND，dcc 树 2 条（`arch/x86/boot/startup/map_kernel.c:49` 的 `if (sme_get_me_mask())`）| `function_inline` 先把调用点展开成语句块里的 `__inl0$rv` 临时量（`VarDecl` **无初值** + `Assign{op: Add}` 写常量，条件位置读的是那个嵌块局部），所以排在后面的 `fold_const_return_calls` 已经**看不见 `Call` 节点**；`propagate_const_locals_in_conditions` 也够不着（不是函数级局部、写入是 `+=`）。`DCC_DUMP_PASS=fold_const_return_calls` 在 `/tmp/kwred/w.c` 上直接读出该形状。五秒夹具矩阵（`/tmp/kwred/pi_{a,b,c,d}.c`）：字面量 `if (0)`、「`!IS_ENABLED()` 早退 + 不可达尾部」、`static`（无 inline）零桩坐 `while` 三形**已能消**；`static inline` 零桩/`_Bool` 返 false/返 NULL 三形 gcc=0、快照=1 | ✅ **已修调度**（2026-10-30：`fold_const_return_calls` 提到 `function_inline` **之前**（仅 `level > 0`，`-O0` 保留原契约），内联后再跑一次兜底；判据不加新规则。夹具加形状 8/9（`static inline` 零桩坐 `if` + 恒 1 反向对照），期望名单拿 `gcc -O2` 实测：只剩 `fs_guarded_keep`/`sme_kept_ref`/`trap_dep_loop`/`trap_live_loop` 四条，而快照留 6 条（多 `fs_guarded_free`+`sme_guarded_ref`）= 当前的红。待新二进制验 `pi_d`/夹具/`map_kernel.o`/`ioremap.o`/`truncate.o`/`direct-io.o` + 整套）|
| 常量桩折叠只认整型返回值 | `fs_bio_integrity_*`（18 行）与 `blk_crypto_*`（14 行）两类未定义引用：`CONFIG_BLK_DEV_INTEGRITY` 未开时 `include/linux/bio-integrity.h:88` 是 `static inline struct bio_integrity_payload *bio_integrity(struct bio *){return NULL;}`，它守着一个全树无定义的调用；gcc -O2 消得掉、dcc 消不掉 | 自己上一轮写 `fold_const_return_calls` 时加的保守限制「指针类常量不拆，本族要拿下的桩全整型」——它把指针返回的 config-off 桩整族漏了（五秒夹具 `/tmp/kwred/bip_stub.c` 实测：gcc 0 条引用 / 旧实现 1 条；反向对照「桩返非 NULL」两边都是 1 条） | ✅ **已修**（2026-10-30：`is_scalar_result_for_stub` 允许整型/布尔/指针；替体改用「返回表达式本身的克隆」（必要时套一层到调用点结果类型的 `Cast`），不再自己拼 `Expr::Int`——`return NULL;` 本来就带目标指针类型。`eval_full` 只认 `Int`/`Addr` ⇒ `return seed;` 这类依赖形参的桩仍不入围。新判据 `const_return_stub_guards_only_kill_the_dead_references`（7 形双向：4 条必须消失 + 3 条必须存在）；待全树重跑后给 `fs/iomap/direct-io.o` 的实测数字）|
| 常量存储前代把「赋值表达式的值」按原始 RHS 记 | `check_opt_equivalence.sh` 的 12 条 OPT-DIFF 之一：夹具 `chained_assign_truncation.c` 在 -O1/-O2 `abort()`（rc=134），而 -O0 正常。五秒复现 `u = uc = 300; if (uc != 44 \|\| u != 44) abort();` —— 打印出来的值就是 44/44，错的是**条件被折成恒真** | `opt.rs::store_const_stmt` 拿 `const_eval::eval_full` 的答案当「这条 store 存进去什么」，而 `eval_full` 的 `Assign` 臂是给**全局初始化器**用的宽松口径：直接返回原始 RHS，连 `op` 都不看。于是 `u = uc = 300` 被记成 `u = 300`（错失了 C11 6.5.16p3：赋值表达式的值是「存入之后按左值类型截断」的对象值） | ✅ **已修**（2026-11-01：新增 `store_value_ctx` 作这一件事的唯一口径 —— 递归下探嵌套 `Assign`、每层套一次到左值类型的 `Expr::Cast` 交给 `const_eval` 截断（与别处 `(T)` 同源，不另写一份），复合赋值（`op != Add`）与 `discarded_value_needs_code` 说「要代码」的形状一律不入围。定位路径：`DCC_OPT_SKIP` 逐 pass 二分到 `propagate_const_stores_forward` 是唯一能修好它的）|
| `&x` 被当值读下探；初始化器里的取址没算「地址逃逸」 | 夹具 `pointer.c` 在 -O1/-O2 **SIGSEGV**（rc=139），-O0 正常。实测 `int *arr[3] = {&a,&b,&c}` 被编成 `mov $0x1,%eax; mov %rax,-0xb8(%r13)` —— 数组里存的是 1/2/3，`*arr[0]` 当场倒 | 两个独立缺口叠加：① `opt.rs::collect_cprop_disq_expr` 没有 `Expr::InitList`/`CompoundLit` 臂，整棵落进 `_ => {}` ⇒ 初始化器里的 `&a` 看不见，`a/b/c` 留在候选表；② `subst_cprop_expr_scoped` 把 `AddrOf` 与 `Unary`/`Cast`/`Deref` 归在同一组当普通值读下探 ⇒ 产出 `&1` | ✅ **已修两处**（2026-11-01：① 补 `CompoundLit`/`InitList` 两臂，逐 `InitItem` 下探；② `&` 的操作数改走新增的 `subst_cprop_designator`（设计符模式：`Ident` 不动、`&*p` 的 `p` 照常代、`Index`/`Member` 的基址递归走设计符而下标走值），拿不准的形状一律不动 —— 即使上游漏判也不该代坏）|
| `__builtin_constant_p` 的实参被代回；「隔离」口径连源码局部也证 | 夹具 `constp.c`（`int x = 1; if (__builtin_constant_p(x) != 0) return 2;`）-O1 rc=2；修完实参那半后 -O2/-O3 仍 rc=2。拿 gcc 对跑发现 gcc 自己就是档位相关的（-O0=0 / -O1+=2），而本仓的契约是**档位无关**（基准取 -O0） | 两条路各自把语法查询当值读：① `propagate_const_locals_in_conditions` 把 `x` 的声明初值代进实参 ⇒ `constp(Int{1})` 被 `fold_constants` 折成 1；② `ipa_modref` 上一轮加的「隔离」口径（`addr == 0` 就 proven）对**源码局部**也答 proven，与 `-O0` 的答案相左 | ✅ **已修两处**（2026-11-01：① 新增 `is_syntactic_query_callee`，在 `subst_cprop_expr_scoped` 与 `subst_cand_reads_expr` 两个 `Call` 臂都加闸，实参不代；② 隔离口径收窄为 `addr == 0 && is_inline_generated(name)`（只认 `__inl*` 前缀的内联临时量）。内核 `BUILD_BUG_ON`/`compiletime_assert` 那一族押的是内联后的形参临时量，实测收窄后 `mm/shmem.o`/`mm/page_alloc.o`/`kernel/sched/fair.o` 仍 `traps=0 extra=0`）|
| opt-eq 契约没进 `cargo test`，只靠外部脚本手工闸 | 上面三行都是**静默误编译**（不是编不过，而是跑错），而 `check_opt_equivalence.sh` 平时不跑 —— 直到全树跑到链接阶段才暴露 | 那 12 条 OPT-DIFF（4 夹具 × 3 档）一直只存在于脚本输出里，没有一个用例钉住它 | ✅ **已补用例**（2026-11-01：`opteq_contract_fixtures_are_level_independent`（4 夹具 × 四档行为必须与 -O0 一致）+ 反向护栏 `builtin_constant_p_of_inline_temp_still_true`（`static int probe(int n){return __builtin_constant_p(n);} probe(3)` 在 -O2/-O3 必须仍答 1，钉住「隔离口径收窄过头」）。两条都已验证有区分度：旧二进制分别出 134/139/2/4，新二进制全 0）|
| 【已修】未定义**数据**引用发 `STT_OBJECT` 且带非零 size | 不报链接错，但同一族缺陷（UND FUNC）曾让 `vmlinux.o` 以 255 挂掉。实测规模：gcc 树 60 个对象的 184 条 UND **全部** `NOTYPE`；dcc 同规模样本 1417 条 `NOTYPE` + **432 条 `OBJECT`**（全树 647 个对象的 UND FUNC 已为 0，上一轮那 6 处入口确实修到位） | GAS 对未定义引用**从不标类型、也不填 size**（五秒夹具 `/tmp/itcase/undtype.c` 四形：`extern int gdata;`/`extern char gbuf[16];`/`extern void (*gfn)(void);`/`extern struct S {int a;} gstruct;` → gcc 四条全 `NOTYPE size 0`，dcc 四条全 `OBJECT size 4/16/8/4`）。`rel_symbols.rs:172-178` 那个 `ty:` 臂只区分了「函数声明 TLS 对象」，**没先看 `section == SHN_UNDEF`**；同一规则在 `elf/symbols.rs:68`（ET_EXEC 路）上还有一份对未定义函数声明发 `STT_FUNC` 的副本 —— 又是「一条规则两个出处」 | ✅ **已修**（2026-11-01：`section == SHN_UNDEF` 时一律 `STT_NOTYPE` + `size 0`（TLS 也不例外），并把 `symbols.rs` 那份副本改成同一入口；判据把 `undefined_symbol_refs_are_notype_like_gas` 从「UND FUNC 必为空」扩成「任何 UND 不得带类型且 size 必为 0」，夹具补进上面四形，反向护栏（本 TU 定义的全局函数仍须 FUNC）不动。已钉成用例 `undefined_data_refs_are_notype_size_zero`（已从 `#[ignore]` 转正，四形全绿）；另有一条既有用例 `block_scope_extern_takes_address_emits_undefined_reference` 原本把 **dcc 当时的口径**（UND 必须是 OBJECT）当契约钉住，已按 GAS 实证改成「UND 不得带类型也不得带 size」，主体判据（必须留下指 `gt` 的重定位）不变。同批落地：`static inline` 的链接性从 `GLOBAL+WEAK` 改回 `STB_LOCAL`（用例 `static_inline_functions_are_local_like_gcc` 三形双向），以及 `-ggdbN` 旗标（objtool/tools 的 HOSTCFLAGS 用它，以前直接 unknown flag ⇒ 宿主工具一步都编不动）|
| 常量存前代越过控制流边界（**回边** + **多进入边标号**） | 全链只用 dcc 后撞到的第一个真误编译：`make prepare` 里 Kconfig 报 261 条 `unknown statement`，还 `double free or corruption`。最小夹具（20 行 Kconfig）上 dcc 编的 `conf` 报 `input in flex scanner failed`，gcc 编的同源 conf 正常 —— 看起来是 flex 扫描器坏了，实际不是。定位链（五步，每步都是产物级判据）：逐对象归属 `.work_tmp/kwconfbisect.sh`（两向替换，只有 `parser.tab.o` 换成就红）→ 档位矩阵（-O0 绿、-O1 起红 ⇒ 优化器而非前端/后端）→ 逐 pass 二分 `.work_tmp/kwpassbisect.sh`（跳过 `propagate_const_stores_forward` 是唯一能修好的）→ 函数级（两边只有 `yyparse` 尺寸差 23 字节）→ 指令流归一化对比（bug 版少一条 `cmp $0x6,%eax`，即 `if (yystate == YYFINAL) YYACCEPT;` 被折掉） | 两个独立缺口叠在一起，同一个根：这条 pass 沿的是**源码顺序**轴，而那个轴只对直线控制流成立。① **回边**：`yy_state_fast_t yystate = 0;`（声明初值）被代到后面的 `if (yystate == YYFINAL)`，但该读点落在 `yynewstate:`/`yysetstate:` 之后，而 reduce 完会 `goto yynewstate;` 跳回来重跑 —— 第二次进入时 `yystate` 已被 `yystate = yyn;` 改过。旧判据只问「跳入是否早于存储」（为了保住 `copy_fpstate_to_sigframe` 的 `retry:` 那一形），完全没管「跳回来再走一遍」。② **紧邻递表**：`goto L;` 紧接 `L:` 时把常量表延续进去，只在**恰好一条**进入边时成立；`yyreduce:` 同时被 `yybackup` 分支与 `yydefault` 分支（紧邻那条）跳入，两条路上 `yylen`/`yyn` 不同 | ✅ **已修**（2026-11-01：`ast_dispatch` 里同一遍历加一份不聚重的 `goto_edges`（`goto_targets` 回答「有没有人跳」、它回答「有几条进入边」，不另起一张臂表）；`LabelCtx` 新增 `max_src`（认出回边）与 `writes_at`（每格的写集合，因为判定当时 `items` 已被循环可变借用，必须在铺序号时先记），判据为「跳回跨度 (标号, 最远回跳源] 内被写过的名字才摘掉」—— 没被写过的照旧保留，`retry:` 那一形不回到一律清表；紧邻递表加 `FwdAxis::sole_in_edge` 闸；两条判据打包成一个 `FwdAxis` 类型而不是两个参数，免「一个传了另一个忘」。实测：`parser.tab.c` 在 -O1/-O2/-O3 全部与 -O0 同行为；逐对象归属表现在**全 dcc 也是 CONFIG=2 错误=0**；行为级用例 `const_store_forward_respects_back_edges_and_multi_in_edge_labels` 拿旧二进制验过有区分度（旧 -O1/-O2 都 rc=1）。lib 2587 / integration 1263 / `check_opt_equivalence.sh` 0 OPT-DIFF 全绿）|
| 【已修】`char` 的类型身份在两条路径上合流（`typeof` 与写出的类型对同一个 `const char *` 答不兼容） | 宿主工具链上最后一个能看到的硬错：`tools/objtool/builtin-check.c` 四处 `error[E0012]: negative bit-field width`（`OPT_STRING` 展开里的 `check_vtype()` → `BUILD_BUG_ON_ZERO(!cpt(typeof(v), type))` 宽度算成 `-!!1 = -1`）。另两条同根的用例：`undefined_data_refs_...` 时代就记下的「`char` vs `signed char` 互不兼容」（`types_compatible_p_known_gaps` 形状 17） | 两件事叠在一起：① `parse_type_mode` 的 `KwSigned` 分支**只消费关键字不记事实**，于是 `resolve_base` 无法区分 plain `char` 与 `signed char`，只能两者都归 `SChar`；而写出来的类型走另一条路保留 `Type::Char` ⇒ `__builtin_types_compatible_p(typeof(o.s), const char *)` 答 0（gcc 两形都是 1）。② 符号性表里只列了 `SChar`，`char` 一旦保留成 `Char` 就全部漏判 | ✅ **已修（类型身份部分）**（2026-11-01：`KwSigned` 记 `is_signed`，`resolve_base` 新增 `signed` 入参，plain `char` 保留 `Type::Char`（C11 6.2.5p6：与 `signed char` 是不同类型）；同时把「值表示有符号」的判据补回 `Type::Char`：`abi/tyclass.rs::is_signed_int`、`vector.rs` 四处元素符号性表、`ast_cmp_fold.rs::sint_key`。两条回归先红后绿（`specex_base_name_cross_...` rc 14→0、`stack_protector_canary_...` stdout `450 -3277`→`450 51`），lib 2587 / integration 1263 全绿。同日下半（限定符归层）也已修：`-dump-ast` 实测结构字段 `const char *s` 的 CONST 落在字段/指针层（`quals: 1`）而 pointee 是 `quals: 0`，即 `const char *` 被当成 `char *const`；而写出的类型走 `parse_ptr_suffix_with_quals` 会落到 pointee ⇒ 同一个类型两个出处。修法是把 `parse_ptr_suffix` 直接**委给** `parse_ptr_suffix_with_quals`（声明、字段、typedef 与类型名共用一份实现，旧的重复实现已删），并修掉 `..._with_quals` 把 `inner_quals` 加在**每一层** `*` 上的错（那会把 `const char **` 做成 `const char *const *`；必须在 `first` 被推进**之前**抓住它）。修后 8 形矩阵与 gcc 逐项一致（`typeof(o.s)`/`typeof(&o.s)` 对写出的 `const char *` / `const char **` 全部 1），**objtool 的 4 处 E0012 清零，`objtool` 能链接（1.75 MB）**。仍开的只有 `types_compatible_p_known_gaps` 两形（`char` vs `signed char`、数组元素限定符），继续 `#[ignore]` 钉住）|
| 【已修】宿主链接命令行没翻译干净：带参选项的**参数**漏给 ld，`.c` 掉到纯链接分支 | `make prepare` 里 `scripts/sign-file` 失败：`ld: 找不到 ./scripts/include: file format not recognized` + `ld: scripts/sign-file.c: file format not recognized` | 两个独立的错叠在 `dcc-host-wrapper.sh`：① `host_link` 只挡了 `-W*/-f*/-g*/-m*` 四族，`-std=gnu11` 掉进 `*) objs+=()` 被当成对象文件（现场就是那句「无法识别的选项 -std=gnu11」）；② 入口靠 `src="${args[-1]}"` 判「最后一个是不是 .c」，而 kbuild 的链接行最后一个常常是 `-lcrypto`/`-lssl` ⇒ .c 留在参数里直接进了 `ld` | ✅ **已修**（同一份 `host_link` 里一次补齐：① 扫全部参数，`.c` 逐个 `dcc -c` 成临时 `.o` 再一起 `ld`（依赖文件的目标名从临时 `.o` 改成最终输出，否则 fixdep 报 `error opening file: scripts/.sign-file.d`）；② 带参选项的参数与**紧贴写法**的编译期选项（`-Itools/include`、`-D…`、`-Wp,-MMD,…`）全部留给编译步骤（`-Wp` 复用同一份 `translate_wp_arg`，不另写一套），其余 `-…` 一律不进链接行；③ `ld` 直链必须自己补 ABI 支持对象 `crtbegin.o`/`crtend.o`（glibc 的 `atexit()` 引用 `__dso_handle`，没它会 `hidden symbol '__dso_handle' isn't defined`），找不到时用 `DCC_HOST_CRT_BEGIN_DIR` 指定或自己发一个 `__dso_handle` 桩 —— 不把「链不上」当结果。实测：`objtool` 与 `scripts/sign-file` 都由 dcc 编 + `ld` 链出，PATH 上 gcc 陷阱仍零触发）|
| 【已修】`scripts/kconfig/preprocess.c` 在 -O2 被误编译（表达式内联把 `&形参` 代成 `&调用者对象`） | 曾经的硬阻塞：`make syncconfig` 跑不过 ⇒ 没有 `include/generated/autoconf.h`，allmodconfig 一个内核 TU 都编不了。两个复现：① 7 行 Kconfig（`/tmp/pv/v2.Kconfig`：`x = plain-text` + `def_bool $(x)`）→ dcc 版 `conf` 报 `free(): invalid pointer` 核心转储；② 纯 C 驱动（`/tmp/ex/t.c`：`#include "preprocess.c"` 后直接调 `expand_string_with_args("plain-text",0,NULL)`）→ -O0 正常、-O2 段错 | 逐对象归属（带 make 环境的装置 `.work_tmp/kwconfenv.sh`）→ 只有 `preprocess.o` 换回 gcc 版就全绿；逐 pass 二分 → 唯一 culprit = `function_inline`；flavor 差异是关键判据（`:=` 不炸、`=` 炸）。根因：**表达式级内联器 `subst_params_mut` 在 `Expr::AddrOf` 臂下直接把形参换成实参表达式** —— `expand_string_with_args(const char *in){ return __expand_string(&in, ...); }` 内联进 `variable_expand` 后，第一个实参在 AST 里变成 `AddrOf{ Member{ v, field: "value" } }`（已用 `DCC_DUMP_PASS=function_inline` 直接读到），于是 `__expand_string` 里那句 `*str = p` 把 `v->value` 改成了循环结束指针（+10），后面 `variable_all_del()` free 它就是 invalid pointer | ✅ **已修**（2026-11-01：给替换 walker 串上「当前是否在 `&` 下」的标志（新增 `SubstCtx { shadows, in_addr, needs_slot }`，仍是**同一张臂表**，不另起一份），`&形参` 时报 `needs_slot` ⇒ 调用方（`substitute_call` / `try_inline_variadic_pack`）直接放弃这次内联并走 `trace_inline_decision` 留痕。语句级内联本来就把形参存进 `__inlN$_p` 局部，不受影响。实测：`make syncconfig` **rc=0**、Kconfig 错 0 条、`autoconf.h` 已生成（598 KB），在 PATH 上摆 `gcc` 陷阱脚本（一被调用就退 99）跑完全程 **零次 gcc 调用**；驱动 t.c/g.c 三档全绿。行为级用例 `inliner_must_not_bind_address_of_parameter_to_caller_object`（拿 `always_inline` 的单 `return` 包装函数复刻同形，旧二进制 -O1/-O2/-O3 全返 1、新返 0；**去掉 `always_inline` 就不复现** —— 因为那时走的是本来就把形参存进局部的语句级路径）。还剩一项：`scripts/mod/devicetable-offsets.c`（见下一行）。已修的两项：objtool 的 E0012（根因是 `const` 落到了错误的指针层，见上面 `char` 那行）与 `scripts/sign-file` 的链接行翻译）|
| 【已修】预处理器把**未闭合的块注释**当成 directive 已经结束（宏体内跨行注释） | allmodconfig 的 `make prepare` 倒在 `scripts/mod/devicetable-offsets.s`：连片 `error[E0003]: expected type, got RBrace/KwWhile/LParen/Number/RParen`，坐标全是 `./include/linux/printk.h:441:498` 这种宏展开尾列；同时 `__printk_index_emit` 的调用点被挖空成 `do { if (...) {;` | `logical_lines` 只按「行尾是不是 `\`」切逻辑行。而 `include/linux/printk.h` 的 `__printk_index_emit` 在宏体里夹了一段跨行注释，**注释中间行不带** `\` —— gcc 把未闭合注释视为「逻辑行未完」继续累积，dcc 却在 `/*` 那行就结束 `#define`，后半截宏体以游离 token 落进文件作用域 | ✅ **已修**（2026-11-01：`logical_lines` 加上「是否在块注释里」的状态，新增 `scan_line_comment_state`（跳字符串/字符字面量、`//` 吃掉行尾；`*/` 闭合后同一行末尾的 `\` 仍算真续行）；摘 `\` 的判据只看扫描结果，不按「本行开头在注释里」一律保留，否则 `*/ \` 那行的 `\` 会被泄进输出。用例 `unterminated_block_comment_continues_the_directive` 用 printk.h 的原字节形状钉住。**附带一个必须记的陷阱**：修好后真树仍报同一个错 —— `~/.dcc` 里 12 GB 旧预处理缓存在回放旧输出；手删才过。于是把「编译器二进制身份」从纪律条款变成机制：`incremental::hash_compiler_binary`（单份实现）同时喂给 compile 缓存与 include 缓存，预处理缓存版本号 bump 到 V14。`devicetable-offsets.s` 产出 39,594 字节，`make -k prepare` 零错误且 `GCC-DELEGATE`/`GCC-USED` 计数为 0 —— 宿主工具链（kconfig/fixdep/modpost/objtool/sign-file）至此全部由 dcc 编、`ld` 链）|
| 【已修】jump-label 的 `asm goto` + `"i"(符号地址)` 不能编码（`arch_static_branch`） | allmodconfig 全树第一批失败 TU 的根因：`error[E0311]: 2 inline asm instructions cannot be encoded`，站点 `arch/x86/include/asm/jump_label.h:37`，模板是 `JUMP_TABLE_ENTRY("%c0 + %c1", "%l[l_yes]")` + `: : "i" (key), "i" (branch)`；实测一轮里 39 个 CC 有 **13 个**倒在同一处（`net/core/{sock,skbuff,datagram,scm,secure_seq}.c`、`samples/vfio-mdev/*`、`samples/ftrace/ftrace-direct*`、`samples/connector`、`samples/trace_events`、`samples/rpmsg`） | 一共三层，前两层在上一轮就已经打通，**真树的第三层完全不长在 asm 上**：① `"i"` 得接受链接期常量（符号地址）——`eval_asm_operand` 走的 `const_eval::eval_full` 已能出 `Addr{name, offset}`，`%c0` 打 `sym+N` 由 `format_asm_symbol_immediate` 承担；② 带 `asm goto` 的 `__always_inline` 体得能在语句位置展开，且展开产生的形参临时量（`__inlN$_key`）值得**代进立即数约束的 asm 输入**：`subst_cprop_stmt` 补 `Stmt::AsmGoto` 臂，`collect_cprop_disq_stmt` 对「裸 `Ident` + immediate-only 约束」豁免（旧规则把 asm 里所有名字一律判 hazard，trace 证据 `FWD| fn=use hazard=["__inl2$_branch"]`）；③ **KCOV**：`needs_edge_coverage()` 把 `kcov` 也算进去，于是 `-fsanitize-coverage=trace-pc` 除了 PC 回调还额外发一条 `trace-pc-guard` 的边计数器，而 `guard_array_decl` 是**往函数体里**塞 `static unsigned int __dcc_cov_guards_N[16]`；`body_is_splicable` 一见 `static` 局部就判「体不可整体搬移」（`DCC_INLINE_BLOCK_TRACE=1` 直接读出 `BLOCK-var| VarDecl { ty: Array(UInt, …) }`）⇒ `arch_static_branch` 根本不被展开 ⇒ `"i"(key)` 拿到的是形参 | ✅ **已修**（2026-11-02：`trace-pc` 与 `trace-pc-guard` 拆成两个旗标（新增 `cov_trace_pc_guard`，`needs_edge_coverage()` 不再收 `kcov`，与 gcc/clang 同口径）；另修 `type_has_vla_dim`——`ArraySize::Var` 只是「维度写成一个表达式」，能 `const_eval::eval` 折的就是定长，不该当 VLA 否决内联（`guard_array_decl` 的 `ArraySize::Var(Int{16})` 正是这一臂）。复现链记在 `.work_tmp/repro/`：`jl7.c`（不带 `always_inline` 是假绿）、`jl8.c`（dyndbg 形状：静态局部 + 嵌套成员地址 + `__builtin_types_compatible_p` 分派）、`jl9.c`（tracepoint 形状：extern 全局 + 偏移 8）。实测：真树 `samples/rpmsg/rpmsg_client_sample.o` **rc=0**（40.7 s），`__jump_table` 已生成；lib 2589 / integration 1266 全绿（含两条新用例 `const_propagates_into_immediate_only_asm_inputs`、`kcov_trace_pc_keeps_always_inline_jump_label`，以及 lib 侧 `kcov_trace_pc_does_not_inject_guards`））|
| `.quad X - .` 发成绝对的 `R_X86_64_64` | 不报任何错，但 `__jump_table` 的 key 字段值错：GAS 对 `.quad sym - .` 发 **`R_X86_64_PC64`**（实测 gcc 的 `jl9.gcc.o`：`.quad __tracepoint_read_msr+8 + 0 - .` → `R_X86_64_PC64 __tracepoint_read_msr + 8`），dcc 发 `R_X86_64_64` ⇒ 存进去的是绝对地址而不是「key 相对本条表项」的偏移，运行期 `jump_label` 会把 `static_key` 指针算到别处（切静态键时砸错地址） | 同一个表达式解析器不看字段宽度：`parse_asm_data_expr` 对 `X - .` 与 `X + N` 都回 `PC32`，而 `.quad` 的调用方把返回的类型**直接丢掉**、写死 `R_X86_64_64`（`elf/output.rs`）；另一条留在 `.text` 的路（`asm/directive_emit.rs::emit_asm_code_data_directive`）则不分宽度一律写 `R_X86_64_PC32`（8 字节字段只低 4 字节有重定位，高 4 字节摸空） | ✅ **已修**（2026-11-02：新增 `asm_expr_has_current_addr`（跳过引号内、要求 `.` 独立成项，`1.5`/`.Lx` 不算）作唯一的「带不带当前地址」判据；`.quad` 路按它选 PC64/R_X86_64_64，`emit_asm_code_data_directive` 按字段宽选 PC8/PC16/PC32/PC64。实测 `jl9` 第三条变 `R_X86_64_PC64 __tracepoint_read_msr + 8`，与 gcc 同；判据进 `kcov_trace_pc_keeps_always_inline_jump_label` 的三字段表项断言）|
| 【探针不诚实】`cc-option` 的空输入探针被当成「没有源文件」交给 gcc | Kconfig 的 `def_bool $(cc-option,-fsanitize=bounds-strict)` 回答「支持」，`CONFIG_UBSAN_BOUNDS_STRICT=y` 一直挂在 `.config` 里；旗标递到真编译后 dcc 直接 `dcc: error: unsupported sanitizer: bounds-strict`（rc=2，每个 UBSAN TU 都碎在命令行上）。同时 `scripts/cc-version.sh` 一类探针也依赖这条路 | `scripts/Kconfig.include` 的 `cc-option` 形状是 `$(CC) -Werror $(1) -c -x c /dev/null -o .tmp_$$/tmp.o` —— 输入是 `/dev/null`，而包装器只把 `*.c`/`*.S` 认作源文件，于是落进 `delegate_gcc "no-source-file"` 那条**遗留的 gcc 代工路**：探到的能力是 gcc 的，dcc 自己做不到 | ✅ **已修**（2026-11-02：`dcc-kernel-cc.sh` 在 `no-source-file` 委托前先认 `/dev/null` 与 `-`（`DCC-PROBE reason=empty-input`），一律交 dcc 自己做。实测 `-fsanitize=bounds-strict` 探针现在 rc=2（诚实的「不支持」），`-fsanitize=bool` 探针 rc=0；删掉 `.config` 里那三行 `CC_HAS_UBSAN_BOUNDS_STRICT`/`UBSAN_BOUNDS*` 后 `olddefconfig` 重推 ⇒ `CONFIG_UBSAN_BOUNDS` 依 Kconfig 自己的 `depends on` 关掉，旗标不再递进来；`make syncconfig` 日志里 `GCC-DELEGATE`/`GCC-USED` 计数 0）|
| `-fsanitize=<子项名>` 一律「不认识就硬错」 | 内核 `scripts/Makefile.ubsan` 按 `CONFIG_UBSAN_BOOL`/`CONFIG_UBSAN_ENUM` **无条件**递 `-fsanitize=bool`/`-fsanitize=enum`（这两项没有 `cc-option` 探测可退），dcc 报 `unsupported sanitizer: bool` ⇒ 每个 TU 一步都编不动 | 解析器只认 `undefined`/`address`/`kernel-*` 那几个粗名字，GCC/Clang 的细粒度子项名全部落进 `_ => Err` | ✅ **已分三类修**（2026-11-02：① `bool`/`enum` 收下但记账 —— 新增 `ubsan_accepted_only` 位图（`SanitizerFlags` 是 `Copy`，所以用位不用 `Vec`）+ `accepted_only_note()`，`driver.rs` 在插桩点发 **W1017** 警告「已被接受但未插桩」；**不**映射到 `ubsan`：dcc 的 UBSan 只有一个开关，一开就是 `type_mismatch`/`alignment` 全家，那是配置没要的运行期行为改变；② bounds 一族（`bounds`/`bounds-strict`/`array-bounds`/`local-bounds`）**故意继续报错**，让 Kconfig 的 `cc-option` 探测把 `UBSAN_BOUNDS` 关掉——这正是 Kconfig 设计好的退路，比「假装支持但什么都不做」诚实；③ `shift`/`integer-divide-by-zero`/`null`/`alignment`/`unreachable`/`signed-integer-overflow`/`undefined` 等现有插桩能对应上的 → `ubsan`。未知名字仍硬错（既有用例 `unsupported sanitizer: not-a-sanitizer` 不动））|
| 【已修】表达式位置的 `static inline` 不展平 ⇒ 外带副本（**试过放宽，已退回**） | 同一个 `.work_tmp/repro/inline_shapes.c`：四个形态（多语句+分支 / 带循环 / `v += f(y)` / if-else）dcc 编出 6 个函数体、.text 1661 B，gcc 只留有地址被拿走的那 1 个、427 B。真树同一条链：`init/main.o` 308 个 FUNC vs gcc 35 个（`.work_tmp/kwdeadfunc.py`：其中 195 个没有一条重定位引用它们） | 调用点落在非语句位置（`return f(y)*2`、实参里）时三条语句级通道一条都接不到 ⇒ 每处留「一份副本 + 一次真调用」；`inline_forced_call_as_stmt_expr` 当时只接 `always_inline`/`flatten` | ⛔ **放宽已退回**（2026-11-02：把成本线接上后放宽，夹具从 1661 B 降到 1275 B 且与 gcc 行为逐字节相同 —— 但回归直接打回来：`check_opt_equivalence.sh` 从 0 OPT-DIFF 变 **10 条**（c_stmt_expr / c_typeof / c_va_arg_int128_loop_local_full_width / opt_planned_frame_slots / opt_switch_selector_effects 各 ×-O2/-O3），`--test integration` 同时挂 3 条（`frame_slots_are_planned_not_lazily_guessed`、`switch_selector_effects_match_gcc_at_all_levels`、`o2_compound_assign_and_decl_init_shapes_expand_under_gate`）。结论：不是成本问题，是这台展开机器在表达式位置还接不住 `__builtin_va_arg`、`typeof`、switch 选择子的求值序与帧内槽位规划——那五形各自修好前不能开口子（开着就是把「少一个优化」换成「静默误编译」）。证据与待修清单已写进 `src/opt.rs` 那条通道函数体的注释）|
| 【已修】SIB 寻址的 asm 指令一律「不能编码」 | 真树失败 TU `arch/x86/kernel/cpu/resctrl/pseudo_lock.c`：`error[E0311]: unsupported inline asm instruction: 'mov (%rbx,%rcx,1), %eax'`；最小复现里 `mov/lea` 的**所有** `(%b,%i[,scale])` 形都挂，而 `(%rcx)` 单基址的没事 | 不是编码器缺 SIB —— `emit_asm_load/store` 早就发得出 SIB。真正的原因在**切操作数**：`emit_asm_mov` 那一路用 `operands_str.split(',')`，而 `(%rcx,%rdx,4), %rax` 里操作数本身就带两个逗号，一条指令被拆成四段，于是没有任一个臂能匹配 | ✅ **已修**（2026-11-02：切分改用的 `split_asm_args` 加上括号/方括号深度判断（不另写一份「谁是分隔符」的定义，数据伪指令原本就用这一份）。产物与 GAS 逐字节对拍：`mov (%rcx,%rdx,8),%rax`=`48 8b 04 d1`、`mov 0x8(%rcx,%rdx,4),%rax`=`48 8b 44 91 08`、`lea (%rcx,%rdx,4),%rax`=`48 8d 04 91`、省略 scale 与写 `,1` 编成同一串 `48 8b 04 11` ✓）|
| 【已修】内联 asm 的助记符**大小写敏感**（GAS 不敏感） | `__asm__ volatile("WBINVD")` 与 `__asm__ volatile("ENCLS")` 都报「不能编码」，消息里还带着原文大写 | 派发表按小写写键，而比对用的是用户原文 | ✅ **已修**（2026-11-02：在 `gas_line_full` 构造**之后**只把派发用的名字折小写 —— `-S` 的输出仍保留用户原文写法）|
| 【已修】SGX 的 ENCLS/ENCLV/ENCLU 不在指令表里 | 真树 4 个 TU 硬错（`arch/x86/kernel/cpu/sgx/{encl,ioctl,main,virt}.c`）：`unsupported inline asm instruction: 'encls'`（站点 `sgx/encls.h:169/175/181/194/208`） | 编码表里没有这三条（它们无操作数，叶子号在 %eax） | ✅ **已修**（2026-11-02：字节串拿 GAS + gcc 产物对拍得出 `encls 0F 01 CF`/`enclu 0F 01 D7`/`enclv 0F 01 C0`。**记账一条方法教训**：先按记忆写成 `0F 01 EE/EC/F0`，对拍当场拒——`0f 01 ee` objdump 标的是 rdpkru；指令编码一律以工具链产物为准，不凭记）|
| 【记账】`__gnu_inline__`/`extern inline` 完全没语义实现 | 一个 `samples/rpmsg/rpmsg_client_sample.o` 里就有 **16 个 WEAK FUNC**，其中 28 个 FUNC 无任何重定位引用（`fortify_memcpy_chk` 440 B、`strcpy` 251 B、`strcat` 248 B）；上面那行「多发 764 个全局 WEAK」就是同一族 | `grep -rn "gnu_inline" src/` **零命中** —— GCC/Clang 的语义是「`extern inline` 的定义**永不**外联发射，调用要么折叠要么引用外部强符号」，而 dcc 把它们当普通函数发一份 WEAK 外联副本 | ⏳ **待修**（`src/ast.rs` 加 `is_gnu_inline`、parser 认该属性、发射侧跳过定义。不能与上一行合并修：那需要先让表达式位置的展平真的能接住这些调用点，否则不发定义就是链接期 undefined reference）|
| 【已修】预处理写盘放大 58 倍（逐层都落条目） | 冷读一个内核大 TU 要 143 s，而它真正的展开只值 ~7 s。量出来的不是猜测：`net/core/skbuff.c`（输出 7.3 MB）一次冷跑让 `~/.dcc/cache` 新增 **5964 个条目 / 408 MB**（1300 个 include 却有 5964 条，因为同一文件在不同嵌套深度各有一份） | 条目的内容是「该文件**连同全部嵌套 include** 的展开结果」，而旧实现**每一层都落一份**：嵌套层的 blob 把子树又抄了一遍，总量是 O(文件大小 × 深度)。这些深层条目基本没人读（回放深度 1 的条目时嵌套部分已经内嵌在内，include guard 又保证同一文件一个 TU 内只展开一次），代价全是白付的字符串拼接 + 序列化 + 写盘（`perf` 里内核侧 `_copy_to_iter`/`kernel_init_pages` 与 malloc/free 系列就是在吃这个） | ✅ **已修**（2026-11-02：`include_resolved_file_inner` 里用 `owner_level = self.include_level`（进入子文件**之前**的深度）作门槛，只有 `owner_level == 0`（主文件直接 include）才算 `macro_hash`/算 `cache_path`/clone 宏表/落条目；顺带省掉每次 include 一次的 `hash_macro_state()`（排序+哈希全部宏）与整张宏表 clone。实测冷读 142.95 s → **6.99 s**，新增条目 5964 → 497，缓存增量 408 MB → 45 MB，**输出字节与旧实现逐字节相同**（去掉 `(cached)` 记账行后 `cmp` skbuff 7,287,666 B 全同））|
| 【已修】每个 TU 的预处理缓存键都不同 ⇒ 跳 TU 零共享 | 旧版冷读同一个头文件集要重展开 N 遍：allmodconfig 速率实测 1.2 min/TU ⇒ ~1 万个对象要 8.5 天 | `hash_macro_state()` 把**全部**宏的名字+参数+体烘进键里，而 kbuild 给每个 TU 递不同的 `-DKBUILD_BASENAME/MODNAME/MODFILE/__KBUILD_MODNAME` ⇒ 键逐 TU 不同 ⇒ ~1300 个头部条目全 miss | ✅ **已修（不靠抹掉差异）**（2026-11-02：新增 `TU_IDENTITY_MACROS`（四个 KBUILD_*），它们的**值不再进键**，只留「定没定义」；正确性改由条目自带的 `IDREAD` 段兑：展开时真读到过哪个身份宏（`note_identity_read` 接在 `#ifdef/#ifndef/#elifdef/#elifndef`、`defined()`、宏展开、`__has_*` 六类读点上）就把「名字 + 当时定义指纹」落盘，命中时 `identity_reads_match` 逐项校，子条目的读数通过 `adopt_identity_reads` 并入父层（父条目也因此不会拿着跳 TU 的内嵌值）；同一名字在本层读到两个值 → `conflicted` → 该文件不落盘。键版本号 V14→V15（旧条目自然作废，无需手工清）。实测：**跳 TU**（skbuff 的条目给 sock 用，KBUILD_* 值不同）4.54 s、61 个命中行，输出仍与旧实现逐字节相同；真读身份宏的 `idh.h` 绝不复用（否则就是 `include/rdma/ib_verbs.h:3690` 把 A 模块名塞进 B 模块的误编译）。双向判据已钉成用例 `preproc_include_cache_shares_only_files_that_ignore_identity_macros`（新文件 `tests/integration/c_pp_cache.rs`：反向钉「值不得串」，正向钉「没读身份宏的文件必须真的命中」——只钉反向的话「什么都不敢复用」也能全绿）。同批产物级对拍：`samples/rpmsg/rpmsg_client_sample.o` 新旧字节相同（39,584 B）；lib 2589 / integration 1267 全绿）|
| 预处理热表的哈希开销（同批做的两件，单独看**不省时间**） | `perf` 冷读 `net/core/skbuff.c`：`core::hash::sip::Hasher::write` 24.8%、`memcmp` 9.1%、`memmove` 4.2%、malloc/free 系列 ≈8% | `std` 默认 `RandomState` 走 SipHash-1-3（为对抗恶意键设计），而预处理器在每个标识符一次 `macros.get(name)` 的量级上跑；`cache_deps_fresh` 又对每个缓存条目的依赖闭包全量重读重哈希（一次编译内同一个头文件要读几百遍） | ✅ **已修两件**（2026-11-02：① 新增 `fast_hash::{FastStrMap, FastStrSet}`（FNV-1a）接在宏表/`pragma_once`/`once_guard`/`include_content_cache`/`poisoned_ids` 等进程内热表上，边界写在模块头：只给进程内表用，**落盘键与增量指纹继续用 `DefaultHasher`**；② `cache_deps_fresh` 改成同一个路径本轮只读一次（`dep_fresh_memo`），`deps`/`dep_hashes` 的去重从线性扫表改集合。产物不变：与旧实现对拍 7,287,500 字节的 `.i` 逐字节相同。这两件本身没降低冷读时间（当时真正卡的是写盘放大），但它们也是 O(n²)，在上面两件修完后的基数上仍值钱。量测装置：`/tmp/{coldcmp,coldone,eqcheck,perfcold,v15eq,sharetest,cachesize,objab}.sh`（冷读对比/产物对拍/perf/跳 TU 共享/缓存增量/对象字节 A-B），`.work_tmp/{kwphase,kwcapturecc,kwrealtu,kwflagbisect}.sh`（分阶段计时/捕获真实 kbuild argv/直跑包装器/旗标集合二分））|
| 【待修】某些 TU 在前端就被 OOM 杀掉（**当前会卡住全树的一个**） | 真树 `net/core/skmsg.c` 直接 `rc=137`，内核日志给的是 `Out of memory: Killed process … (dcc-tree) total-vm:9187164kB, anon-rss:6440116kB` —— 单 TU 吃到 6.4 GB RSS，而这台机器只有 7.9 GB（`-j2` 两个任务就满了）。失败不取决于负载：空闲 6.1 GB 时单跑一个目标仍被杀 | 分阶段量：预处理只 **3.7 s / 202 MB**（本轮预处理修完后的数字）；而 `-fsyntax-only`（只走到 parse+sema，不碰优化器与后端）单独就要 **7.05 GB / 39 s** ⇒ 质量不在 codegen，在**前端**。再看规模曲线（`.work_tmp/kwmemclip.sh` 逐段截 `.i`）：45k 行 88 MB、90k 行 282 MB、100k 行 297 MB、130k 行 223 MB、135,700 行 239 MB，而全量 135,752 行是 **7,056 MB** —— 不是平滑的超线性，而是在**最后 52 行里有一道悬崖**；那 52 行里正是 `__compiletime_assert_1801…1805` 那一族 `READ_ONCE`/`data_race` 宏展开成的超长行（每行几 KB，内含嵌套 `({...})` 与一串 `sizeof(…) == sizeof(char) \|\| …`） | ⏳ **待定位**（下一轮从这里接手：拿 `perf record --call-graph` 配一个不 strip 的 release 二进制跑 135,752 行那份 `.i`，看 7 GB 的分配落在哪个函数上；量测装置已同定：`/root/dcc/.work_tmp/{kwmemphase,kwmemclip,argv_skmsg.bin}`，夹具 `.i` 可由 `dcc <真实 argv> net/core/skmsg.c -E` 重现。先判「哪一类形状」再写最小夹具，不要拿整份 `.i` 调）|
| 【记账】函数作用域 `static` 变量被 asm 取址时引用到未定义的同名符号 | `nm` 读 `jl8.dcc.o`：静态局部 `descriptor`/`desc2` 同时存在两条——一条 `d .Lsl0.use.descriptor`（在 `.data`，本体），一条 **`U descriptor`**（未定义），而 `.rela__jump_table` 的重定位指向后者；gcc 的口径是把静态局部发成 `descriptor.1` 这类**带后缀的 LOCAL 符号**（实测 gcc 的 `.s`：`.long descriptor.1+20 + 0 - .`）。当前真树还没撞上（dyndbg 的 `_ddebug` 静态量走的是 `__dyndbg` 节 + `__UNIQUE_ID_*` 名字，实测 `rpmsg_client_sample.o` 的 key 字段符号是 `__UNIQUE_ID_ddebug_759`），但同名静态局部在两个函数里出现时会塌成一个符号 ⇒ 潜在多重定义/错对象 | `const_eval::eval_full` 对静态局部的 `Addr{name}` 用的是**源名**，没接发射侧的 `.LslN.<fn>.<var>` 内部名；`reloc_sym_index` 拿这个名字查符号表查不到就发 UND | ⏳ **待修**（不挡当前构建；修法定在「地址常量求值必须返回发射侧那一份身份」，即让 `eval_full` 走 `static_local_symbol(fn, var)` 的唯一出口，别在重定位侧再拼一次名字。另记：`__jump_table` 的 key 字段是 8 字节绝对还是 PC 相对，取决于 `CONFIG_HAVE_JUMP_LABEL_BATCH`/`_ASM_PTR` 的写法，改动后需拿 gcc 参照树同 TU 对拍）|

三条实测依据（都是同一台机器、同一 `-j` 档跑的）：

**速度的下一块石头已经量出来了（11-02）**：预处理修完后，一个样本 TU 的端到端是 18.4 s；把同一份
`.i`（55,355 行）拆开测：`-fsyntax-only` 1.86 s、`-O2 -c` 16.26 s，而 **`DCC_OPT_SKIP=all` 下仍要
18.2 s** —— 优化器不是成本，**IR lower + codegen + ELF 发射那 ~14 s 才是**（≈0.25 ms/行，而 gcc
整个 TU 约 1.5 s）。这就是下一个目标；不先动它的话，~1 万个对象仍要按小时到天的量级算。
另一条顺便量到的事实：`-O0 -c` 对带 jump-label 的形状直接 `rc=1`（语句级「整体搬移」那套展开
机器 `level < 2` 不开，见上面 jump-label 行的注释）——内核走 `-O2` 不受影响，但这是个已知的档位边界。

- 形状矩阵：`while`/`do-while` 条件、语句表达式在 if 条件/声明初值/表达式位置三种摆放，修前
  `rc=1 error[E0311]`、修后 `rc=0`，且产物里没有 callee 的独立副本符号；`for (;f();)` 修前就是好的
  （这正是「同一个形状在两条路径上不一致」的信号）。
- 运行期与 gcc 对拍：计数器 `n_calls` 逐轮递增的夹具（能抓住「展开被错提到循环外」）在
  `while`/`do-while`/`for`/语句表达式/实参嵌套五种形状上与 gcc -O2 退出码逐字相同。
- asm 体内 `call` 的守护到了产物：`objdump -d` 里 `call` 前后各有一对 `push`/`pop`（r8/r9 两个
  延伸池号），被调方清空 caller-saved 后主调方仍与 gcc 同值。

另一处与「产物必须由当前编译器编出」直接相关的加固：`incremental.rs` 的缓存指纹以前只覆盖
「源内容 + 影响输出的选项」，重编 dcc 之后仍会命中上一版编译器写下的条目。现在把
`current_exe()` 的 size + mtime 也钉进 key，换二进制自然作废（内核构建默认不开 `-fcached`，
这条保护的是显式用增量缓存的场合；~/.dcc/cache 是**预处理**缓存、按头文件内容哈希，不受影响）。

本轮新发现的发散（不挡构建，记账）：**dcc 的 `endbr64` 比 gcc 发得多**。同一批 TU 上是
xstate 93/24、events core 167/69、bugs 143/53、mm_init 112/29 —— GCC 只在可能被间接命中的
入口放（实践上即外部可见的函数），dcc 目前每个函数入口都放。方向是安全侧（多一个 `endbr64`
不会让 IBT 故障），代价是 `.text` 膨胀、以及与参考产物的字节差异；对齐它需要「哪些函数允许被
间接转移」这个事实进入 codegen，记在 §5.4 已知限制里。

### 真树规模宿主工具验证的两个装置陷阱（2026-11-01）

1. **不能手工跑 `scripts/kconfig/conf --syncconfig Kconfig` 做 A/B**。这条命令依赖 make
   导出的环境（`srctree`、`SRCARCH`、`CC` …）：脱离 make 跑时 `source "arch/$(SRCARCH)/Kconfig"`
   会展开成 `arch//Kconfig`（递归包含告警），`scripts/Kconfig.include` 里的
   `$(shell,/scripts/cc-version.sh)` 路径也缺前缀。两个二进制在这种情况下**分岔点不同**：
   gcc 版在「Sorry, this C compiler is not supported」处提前退出（于是错误行数=0），
   dcc 版继续往下跑才刷出错。拿它做归属就是把「谁先退出」当成「谁解析对」。
   正确做法：要么在 make 里跑（`make syncconfig`，每次只替换一个 `.o` 并删 `conf` 逼它重链），
   要么把 `srctree`/`SRCARCH`/`CC` 先导出齐再比。
2. **小夹具上的两向替换是有效的**（`/tmp/kt` 那 20 行 Kconfig 不碰 `srctree`/`SRCARCH`），
   所以 `parser.tab.o` 的归属、以及修完之后拿旧/新二进制跑 `back_edge()` 看退码（旧 rc=1、
   新 rc=0）这两重证据都成立；被推翻的只是「`preprocess.o` 是下一个 culprit」——
   它源自装置问题，尚未重新归属。本轮只确认：真树 `make syncconfig` 在回边修复后仍有
   330 条解析错 + 一次 `double free or corruption`，需要在带 make 环境的装置下重做归属。

可用的工具：`.work_tmp/kwconfbisect.sh`（小夹具，两向替换）、`.work_tmp/kwconfbig.sh`
（真树，已注明它受上面陷阱 1 影响，不能单独作为结论）、`.work_tmp/kwpassbisect.sh`
（逐 pass 二分，TESTCMD 由调用方给）、`.work_tmp/xsym.awk`（重定位目标名的唯一解析）。

### 构建日志里的诊断族账目（2026-11-01，full36 前 536 个 TU）

只记账不动手：改诊断也要重发二进制，而现在手里跑着的这轮全树必须用**同一版**编译器跑完。
下一轮重开前一并处理。

| 诊断 | 条数 | 是什么 | 当下结论 |
|------|------|--------|----------|
| `W1015` | 4419 | `return type of '__builtin_unreachable' is not known; assuming int` | 噪声主体。内核几乎每个 `BUG_ON`/`switch` 默认臂都带 `unreachable()`，而报的**列号离谱**（1209/1047 这种）—— span 落在宏展开后的物理行尾，这条本身也是个缺陷 |
| `W0501` | 3744 | `implicit fallthrough between switch cases`，建议写 `[[fallthrough]]` | 两重错：建议语是 C++ 的（C 应写 `fallthrough;`），且不认内核已经写的显式标记。纯噪声，不影响代码 |
| `W0003` | 442 | `format '%X'/'%u'/'%s' expects … incompatible type`，集中在 `drivers/acpi/acpica/*` 的 `ACPI_ERROR((AE_INFO, …))` 双括号宏 | 变参实参类型判错（双括号宏把整个 `(...)` 当一个实参），产物无关 |
| `W1004` | 6 | `dereference of non-pointer type`，全部在 `kmalloc_flex`/`kvmalloc_flex`/`struct_size` 那一族里（`block/blk-map.c:29`、`block/blk-mq-sched.c:517`、`init/initramfs.c:156`、`io_uring/rsrc.c:116` 等） | **已排除代码错**：拿同形的五秒夹具（`sizeof(*(p)) + sizeof((p)->m[0]) * n` 套在 `__builtin_choose_expr` 里，`p` = `(struct flex *)0`）对拍，dcc 与 gcc 同值 7，且不复现该警告 ⇒ 另一条子表达式上的误报 |

真正要盯的是链接期的未定义引用；这些警告不影响它，但影响日志可读性（每 TU 几十条）。

### 全树验证节奏：先拿普查判编译器，再起全树（2026-11-01）

上一轮 full34/full35 两次作废同一个原因：**产物掺了两版编译器**。`make` 跑着的时候直接
`cargo build` 会覆盖 `target/release/dcc`，而包装脚本 `scripts/dcc-kernel-cc.sh` 默认拿的就是
这个路径 ⇒ 后半段的 `.o` 是新编译器编的、前半段不是，链接期的未定义引用名单说不清是哪一版
给的。定下的规矩：

1. 试验版一律 `CARGO_TARGET_DIR=/tmp/tgtNN cargo build --release`，**绝不**碰 `target/release/dcc`；
2. 验证只靠它的路径走 `.work_tmp/kwaudit.sh <二进制> <输出目录> <并行> <清单>`（逐 TU 从 gcc
   参照树的 `.cmd` 复原命令行，再 `nm` 扫陷阱符号）；
3. 普查全零才 `cp /tmp/tgtNN/release/dcc target/release/dcc` + `scripts/kernel_purge_stale_objects.sh`
   （两条二进制级判据：mtime 早于二进制、`.comment` 带 GCC），然后从零起全树。

本轮普查结果（全部是 `/tmp/tgt18` 那版，即上面三行修法后的二进制）：

| 判据 | 结果 |
|------|------|
| 119 个现场 TU 普查（`/root/kwlogs/aud119f.log`） | 119/119 `traps=0 extra=0`，零 `BUILD_FAIL` |
| 另测 constp 代回族现场：`mm/shmem.o`、`mm/page_alloc.o`、`kernel/sched/fair.o` | 三个均 `traps=0 extra=0`（证明「隔离口径收窄」没把这族顶回来） |
| 测试用例（lib / integration） | `lib 2586 passed`、`integration 1260 passed`，零失败 |
| 测试用例脚本 `check_opt_equivalence.sh`（全量） | `0 OPT-DIFF`（12 条存量债已清） |
| full36 | 已清 465 个旧 `.o`（含 3 个 gcc 回退产物），`make CC=… -k -j4`，日志 `/root/kwlogs/full36.log`；参照树共 1489 个内核 `.o`，本轮速率 ≈ 6 个/分 |

### 链接期预检不必等全树（`kwmerge.sh`，2026-10-28）

`vmlinux.o` 要所有 TU 都编完才有，但链接期的 objtool 判据只看「对象拼一起后的节布局与
注解坐标」。`.work_tmp/kwmerge.sh` 拿**当前已编出来的这批 TU**先跑一遍真链接前奏：

```bash
cd /root/linux-7.1-dcc && find . -name '*.o' ! -name 'built-in.o' | sed 's|^\./||' | sort > /tmp/built_now.list
nice -n 10 bash /root/dcc/.work_tmp/kwmerge.sh /tmp/built_now.list /tmp/kwmerge
```

同一清单在 gcc 参照树上做同样的 `ld -r`，两边各得一份合并对象，再各自跑
`objtool --link --ibt/--noinstr/--unret/--orc --noabs`。本轮 459 个 TU：**gcc 四项全 ok、dcc 四项全 ok**。
清单要按顶层 `core-y/libs-y` 做**白名单**：`tools/`、`arch/x86/boot`、`arch/x86/realmode`、
`arch/x86/entry/vdso`、`arch/x86/purgatory`、`scripts/` 与目录聚合的 `built-in.o` 都不进 `vmlinux.o`，
混进来必撞重复符号（实测两组冲突：`realmode/copy.o` vs `lib/memcpy_64.o` 的 `memcpy/memset`，
`tools/objtool/arch/x86/decode.o` vs `arch/x86/lib/inat.o` 的 `inat_*`，**gcc 侧同红**）——
那是清单错不是编译器缺陷，只信两边的差异。

### 体积缺陷：`static inline` 助手不外联展平，`.text` 是 gcc 的 6-7 倍（记账，暂不动）

同一批 459 个对象合并后的尺寸对比：`.text` 8 162 380 vs gcc 1 204 515（6.8x），
`.orc_unwind` 1 103 928 vs 6 198，`.rela.orc_unwind_ip` 4 415 712 vs 24 792。
单 TU 抽样：`init/main.o` 的 `start_kernel` 4805 字节 vs gcc 1895；该对象里有 54 个
WEAK FUNC + 32 个 LOCAL FUNC，而 gcc 侧连 `static_key_enable`/`static_key_disable` 都不存。

查到的三条（前两条是同一条链，第三条是顺带发现的独立缺陷）：

1. **主因是优化缺口，不是 DCE 缺口**。`src/opt.rs:112` 写得很明白：O0-O2 只内联
   `always_inline`/`flatten`，启发式内联留给 O3。于是头文件里每个 `static inline` 助手在本 TU
   都留一份完整外联副本 + 一次真调用（实测 `init/main.o` 有
   `R_X86_64_PLT32 static_key_enable`、`… static_key_disable` 两条活调用），而 gcc 直接展平。
2. dcc **确实有**函数级 DCE：`src/opt.rs:162-164 cross_function_dce` →
   `src/ssa/passes.rs:2547 cross_function_dce_with_roots`。上面那些副本并不死、也不该被删（有人调用）。
   但该 pass 自身有三条已定位的缺陷（根集合被死函数的裸 Ident 污染且不迭代到不动点；
   `fold_branches_block` 含 `Stmt::Label` 就拒折 + `fold_branches_stmt` 缺 `Do`/`Switch`/`Return`/`Expr`
   四臂；`alias`/`copy` 持有者自己会被 `retain` 删掉），详见设计档。
3. **链接性偏离（比体积严重）**：`src/codegen/x86_64/elf/rel_symbols.rs:450` 把
   `is_weak || is_inline` 判在 `is_static` 前面 ⇒ `static inline` 被发成 `STB_WEAK` 而不是
   `STB_LOCAL`。同名不同体的两份文件局部 `static inline` 在 `ld` 只会留一份 ⇒ 静默换实现。
   按 C 语义（先 `extern` 声明后 `static inline` 定义 ⇒ 内部链接）与 gcc 行为都应是 LOCAL。

2026-11-01 拿 full36 首批 424 个对象重测了一遍（`ld -r` 后对同一清单比符号集）：

- **全局符号零缺失**：gcc 合并对象里 4052 个 GLOBAL，dcc 全部都有（comm 反查为空）——
  也就是说没有「该发没发」类的引用缺口。
- **多发 764 个全局**，`readelf -sW` 逐项抽看全部是 `FUNC WEAK`（合并对象里共 785 条 WEAK，
  gcc 侧同名单是 `LOCAL`）—— 上面第 3 条的实测规模就是这个数。
- 同一份合并对象尺寸 15M vs gcc 4.8M，与第 1 条（头文件 `static inline` 助手不外联展平）一起记。
- 同期跑的四项 objtool 预检（`--link --ibt/--noinstr/--unret/--orc`）**两边全绿**，这批对象
  在链接前奏这一关没有新缺陷。
- **第 3 条的实时危害面：不是 0，是 296 次活调用**（上一版本行写的「0」是错的，错因见下面
  那段工具陷阱）。拿 `.work_tmp/kwweaksizes.sh` 把这批对象的 WEAK FUNC 按名字分组比尺寸：
  808 个弱名里 **40 个**在不同 TU 里体长不同；拿 `.work_tmp/kwweakcalls.sh` 求「本 TU 既定义
  该弱名、又发一条指它的重定位」：首批 424 个对象里就有 **296 条**命中 ——
  `kmalloc_noprof` 99、`native_save_fl` 83、`cpuid` 20、`test_ti_thread_flag` 14、
  `native_read_msr` 12…（命中最多的对象：`arch/x86/kernel/cpu/mce/core.o` 12 条、
  `arch/x86/kernel/process_64.o` 7 条）。这类名字都是按调用点常量展开的
  `static __always_inline`（`kmalloc_noprof` 折的是尺寸/类型，`native_save_fl` 折的是
  旁路/补丁选项），体与体之间**不等价** ⇒ `ld` 对同名 WEAK 只留一份就是**静默换实现**，
  属于正确性债不是体积债。原先「先打通链路再回头修」的安排不变（现在改就要重跑全树），
  但修完之后那**一次**从 0 复现验证是必须的，且验收得跑 `kwweakcalls.sh` 确认命中归零。

**工具陷阱（本轮实测踩到，已固化成 `.work_tmp/xsym.awk`）**：`readelf -rW` 把带加数的重定位
打成 `… R_X86_64_PC32  0000000000000000 native_save_fl + 0`，`+` 两侧**有空格** —— 拿 `$NF`
当名字得到的其实是加数（`0`、`b1a`），于是「按重定位找引用」几乎全部漏报（`bugs.o` 同一个
解析器从 42 个名字变成 153 个，差 3.6 倍）；而节相对行的 `$NF` 又能是个十六进制串，反过来
造出一批「幽灵名」。正确做法是先用类型列定位、再取「16 位十六进制符号值」的**后一列**；
gcc 侧与 dcc 侧共用 `xsym.awk` 这一份，不再各写一个 awk。

为什么排在打通链路之后：(1) 与 (3) 是同一条链的两端 —— 把 `static inline` 改回 `STB_LOCAL`
之后，头文件助手就不再有全局名字可被 `ld` 互串，上面那 296 条活引用风险随之归零；而两者
都要重发产物，会让已产出的 `.o` 全部作废，跟「不要每轮从 0 开始」直接冲突。**注意本文
以前提的「不挡正确性」已被上面的实测推翻** —— 它挡正确性，只是不挡链接。定在打通全链路
之后与那**一次**从 0 复现验证一起做；
具体改法、要覆盖的 13 类内核引用、15 条用例与反向验证办法在
`report/dcc-static-inline-bloat-and-dce-2026-10-28.md`。

改法已核到行（下一版二进制一起做，与上面的 UND 数据引用同一次重发）：
`rel_symbols.rs:491-497` 的 `bind:` 现在是
`if f.is_weak || f.is_inline { STB_WEAK } else if f.is_static { STB_LOCAL } else { STB_GLOBAL }`，
把「inline」判在「static」前面 ⇒ `static inline` 拿到 WEAK+GLOBAL。按 C 语义
（`static` 一律内部链接，与 `inline` 无关）应把 `is_static` 提到最前。已核过：
**没有任何现有用例把 `static inline` 钉在 WEAK 上**（只有一组用例钉 `__attribute__((weak))`
必须 WEAK，它们在 `is_static` 为假的那一支，不受影响），所以这条改动不会碰既有期望；
新增判据应至少盖三形：`static inline` 发 LOCAL、非 static 的 `inline`（GNU89/C99）仍发 WEAK、
`__attribute__((weak))` 仍发 WEAK。

## 历史进度（2026-10-26）

本轮只推进「让内核 C 语料能被 dcc 编出」这条链上的**内联与常量求值**缺口；全树跑到第 48 个 TU
被主动中止，**`vmlinux.o` 及其后仍未验证**。逐项数字与失败清单见
`report/linux71-dcc-fulltree-build-2026-10-26.md`（`report/` 不入库，是一份本地一次性产物）。

| 项 | 状态 |
|------|------|
| 「只能接立即数」的约束拿到运行时值 | ✅ 改为当场硬失败，并在诊断里点名表达式（`expression: …`），不再静默发出指向寄存器名的野重定位 |
| `asm goto` 形体的 `__always_inline`（`jump_label.h` 的 `arch_static_branch`） | ✅ 可内联；标签定义、`goto` 与模板里的 `%l[…]`/`%c[…]` 文本一起改名 |
| `static_cpu_has`/`cpu_feature_enabled` 宏链（`cpufeature.h`） | ✅ 三元先折子表达式再判条件；`"i"` 操作数从形参临时量代回实参；`testb $imm, sym(%rip)` 与 GAS 逐字节一致 |
| `__WARN_bug_entry` 的 `__flags` 局部常量（`asm/bug.h`） | ✅ 写一次的常量局部与常量重赋值都能代回，按声明块压/弹作用域 |
| `call_on_stack`（`asm/irq_stack.h`） | ⚠️ 函数设计符 `"i"` 与 `call 符号` 已修；`GP 寄存器池耗尽` 未修 |
| `rip_rel_ptr`（`asm/asm.h:121`） | ❌ 仍有 `expression: p` 站点没展开（缺调用点的值专化） |
| 全树 `make CC=dcc` | ⏸ 第 48 个 TU 处 3 个失败，本轮主动中止 |
| `vmlinux.o` / `vmlinux` / `bzImage` / QEMU | ⏸ 未到达（上一轮挡在 objtool，本轮挡在更早的编译步骤） |
| `make test` | ✅ 3887 passed / 0 failed / 7 ignored；`gcc.c-torture` 3707 pass / 0 fail / 6 skip（pre-commit 闸门实跑两次） |

一条通用结论值得单独记住：**`"i"`/`"I"`/`%c[…]` 这类约束是「链接期常量」的契约，不是优化建议**。
展开/折叠链上任一环把它当成寄存器，产物就带着一条解不开的重定位出门，而编译器自己一声不响；
dcc 现在的立场是：给不出常量就当场硬失败，并把表达式原文写进诊断，让缺口在全树第一轮就能被列齐。

## 历史进度（2026-10-25）

全树重跑与缺陷修复的一轮。结论：**内核 C 语料已 100% 由 dcc 编出（零 gcc 回退），但 `vmlinux.o` 仍未打通**。

| 阶段 | 状态 |
|------|------|
| 全树 `make CC=dcc`：1093 个内核 `.o` | ✅ 全部由 dcc 编出，日志内 error 行 0、`falling back` 0 |
| `built-in.a` 归档 458 个 | ✅ |
| `vmlinux.o`（`ld -r` + objtool） | ❌ objtool 报 120 条 `empty alternative entry` + 1 条 `can't find orig instruction`（ALTERNATIVE 表条目的 `orig_len` 字节为 0）。注意：这份清单是从中间版本二进制编出的 .o 采集的，purge 重跑后需重采 |
| `vmlinux` / `bzImage` / QEMU | ⏸ 被上一项挡住，未到达 |
| `cargo test --release --lib` / `--test integration` | ✅ 2576 / 1187+5 ignored 全通过（本轮新增 12 条用例，逐条做过「撤掉修复即变红」反向验证） |

本轮修掉的 9 类缺陷（症状 → 根因 → 落点）见 `report/linux71-dcc-fulltree-build-2026-10-25.md`；
其中三项影响所有架构的通用正确性：

1. **类型身份与类型相等不能用派生 `Debug`/派生结构相等**：内核类型是「先前向声明、后补全」
   且带环的共享 DAG，派生实现一边把类型串算成 GB 级（编译被 OOM 杀），一边把同一个 tag
   判成两个类型（`EXPORT_SYMBOL_GPL` 的 `extern typeof(sym) sym;` 误报 E0007）。
2. **弱符号与节属性是链接期契约**：`__weak` 在「属性写在 `*` 之后」「只写在原型上」两种写法下
   丢失、函数类型的无体声明被当 tentative definition 发进 `.bss`，都会在 `ld -r vmlinux.o`
   变成 360 条 multiple definition。
3. **自定义节的 flags/对齐必须按 GAS 语义**：`SHF_MERGE` 不得在函数级并行合并时被压平，
   非 merge 节默认对齐是 1 不是 8——否则 `ld` 逐对象垫边界，objtool 的
   「节长必须是条目长整倍数」立刻破（`.altinstructions` 14、`__ex_table` 12）。

## 历史进度（2026-08-27）

> 下表是 2026-08-27 那一次基于当时 HEAD 的记录；其中「全树 vmlinux + QEMU 通过」一行
> 在 2026-10-25 用当前 HEAD 复现不出来（上表是本次实测结论），保留仅作历史基线。

| 阶段 | 状态 |
|------|------|
| DCC release 构建 + `cargo test`（integration + errors snapshot） | ✅ 本地通过 |
| `scripts/test_kernel_features.sh` | ✅ 7/7 |
| `scripts/prepare0_check.sh` | ✅ devicetable / bounds / file2alias 窄目标 |
| `scripts/pahole_regress.sh` / `objtool_regress.sh` | ✅ BTF struct + eh_frame FDE |
| Kbuild `devicetable-offsets.c`（直接 dcc -S） | ✅ 50+ SIZE_* 行 |
| Kbuild `kernel/bounds.c`（直接 dcc -S） | ✅ |
| `arch/x86/kernel/asm-offsets.c` 预处理 + 解析 + 代码生成 | ✅ 2026-07-17：867→0 解析；0 语义；`.s` 已生成 |
| modpost `file2alias.c` 单 TU | ✅ |
| LTO 链接 `-r` 多 `.o` + dead symbol gc | ✅ `ld -r --gc-sections` + `.llvm.lto` |
| `scripts/modpost_link.sh` | ✅ Rust fixdep + dcc 三 TU（modpost/file2alias/sumversion） |
| 全树 `make CC=dcc` / vmlinux / bzImage / QEMU | ✅ **vmlinux + QEMU boot 验证通过**（16MB ELF，串口启动日志正常） |
| **initramfs 启动到 init** | ✅ **已验证**（内核成功执行 "Run /init as init process"） |

### 本地一键验证

```bash
# 在 dcc 仓库根目录执行
bash scripts/test_kernel_features.sh
bash scripts/prepare0_check.sh
bash scripts/pahole_regress.sh
bash scripts/objtool_regress.sh
```

默认在 `./` 仓库内依次运行：

1. `cargo build --release` + `cargo test --release`
2. `scripts/test_kernel_features.sh`（含 `-S` 自定义节 / lock cmpxchg）
3. 内核单 TU：`devicetable-offsets.s`、`bounds.s`
4. `prepare0_check.sh`、`pahole_regress.sh`、`objtool_regress.sh`
5. `kernel_full_check.sh`（modpost / prepare0 / QEMU / vmlinux）

### QEMU 内跑 LTP 冒烟（验证链已于 2026-10-28 跑实）

工具在 `/root/kwqemu/`：`prepare_ltp.sh`（构 LTP）→ `build_initramfs.sh`（把冒烟用例连
`ldd` 依赖一起打进 initramfs）→ `run_qemu.sh`（启内核并跑完输出每用例退出码）。

```bash
# 1) 构建 + 安装 LTP（/root/ltp 是 git 浅克隆，本机无 autotools；当时已拿到 configure）
cd /root/ltp && make -j2 all
make -j2 install DESTDIR=/root/kwqemu/ltp-install SKIP_IDCHECK=1
# 2) 打包进 initramfs（注意：这次 install 的 prefix 是 /root/ltp-install，不是 /opt/ltp）
cd /root/kwqemu && LTP_PREFIX=/root/ltp-install bash build_initramfs.sh
# 3) 跑参考（gcc）内核建立基线；换成 dcc 内核只改 KERNEL=
timeout 400 env KERNEL=/root/linux-7.1/arch/x86/boot/bzImage LOG=/tmp/ref_boot3.log bash run_qemu.sh
```

基线实测：**18 个冒烟用例全部 `exit=0`，TBROK/TFAIL 0**，到 `Run /init` → `DCC-BOOT-OK` → 干净关机；
逐项结果存盘在 `/root/kwqemu/baseline-gcc-kernel-ltp.txt`（含 initramfs 哈希）。之后对 dcc 内核
只比这份基线，不去讨论“用例本身能不能跑”。

清单位置 `/root/kwqemu/ltp_smoke_list.txt` 的两条改动缘由（都写在文件注释里）：原来的
`close01` 靠 `socket()` 造 fd，而这份 `.config` 没有网络栈，在**参考内核上也是 TBROK（ENOSYS）**；
替上的 `chdir01` 又需要真块设备（`tst_device` 拿不到设备），最后换成同族不碰套接字也不碰块设备的
`fchdir01`——18/18 才是这么来的。

对 dcc 内核只需一条命令（已于 2026-10-28 用参考内核把这条 harness 本身跑实）：

```bash
# 启动健康度（panic/BUG/WARNING/attempted to kill init 全部计数）+ 逐用例结果 + 与基线对比
nice -n 10 SMP=1 bash /root/dcc/.work_tmp/kwbootltp.sh /root/linux-7.1-dcc/arch/x86/boot/bzImage
timeout 440 nice -n 10 SMP=1 bash /root/dcc/.work_tmp/kwbootltp.sh /root/linux-7.1/arch/x86/boot/bzImage /tmp/kwboot-selfcheck-ltp.txt
```

一个必踩的坑（已在脚本里处理）：**串口日志行尾是 CR+LF**，基线文件就是直接重定向串口输出留下来的
（`cat -A` 能看到 `^M$`），新产的结果是 LF。不统一行尾，`diff` 会把 18 条全部比成「全不一样」——
那是行尾差异不是行为差异，很容易误判成「内核挂了」。两边都 `tr -d '\r'` 后再比。

### 阶段 B 质量门禁（步骤 7–10）

```bash
# 本地（脚本须 LF；Windows scp 后执行 perl -pi -e 's/\x0d//g' scripts/*.sh）
bash scripts/vmlinux_regress.sh               # 无 vmlinux 时 SKIP
bash scripts/qemu_boot_matrix.sh              # 无 bzImage 时 SKIP；有 bzImage 时硬门禁
BUILD_BZIMAGE=1 BUILD_VMLINUX=1 bash scripts/kernel_build_smoke.sh  # 全树 smoke
STRICT_FULL_BUILD=1 bash scripts/kernel_full_check.sh  # strict: 禁止核心门禁 SKIP
```

`STRICT_FULL_BUILD=1` 会强制启用 `BUILD_VMLINUX=1` 与 `BUILD_BZIMAGE=1`，
并要求 `modpost` 三个 TU 全部由 dcc 编译、`vmlinux` / `bzImage` 产物存在，
且 `pahole` / `objtool` 必须安装并通过检查；上述核心门禁出现缺失产物、
缺失工具、非零退出或复用旧 modpost 的情况都会返回失败，不再按 SKIP 处理。

| 脚本 | 用途 |
|------|------|
| `build_fixdep_dcc.sh` | 安装 Rust `fixdep`（不依赖 gcc 编译 C fixdep） |
| `dcc-host-wrapper.sh` | HOSTCC 包装：dcc -c + gcc 链接；fixdep 走 Rust |
| `gen_elfconfig_h.sh` | 从 empty.o 生成 elfconfig.h |
| `prepare0_full.sh` | prepare0 全链路（等价窄目标 + modpost） |
| `kernel_purge_stale_objects.sh` | 清掉「早于当前 dcc 二进制」与「gcc 回退」两类 .o，使全树产物同一版编出（见 §5.0） |
| `pahole_regress.sh` / `objtool_regress.sh` | 单 TU BTF / eh_frame 回归 |
| `qemu_boot_matrix.sh` | QEMU 加载 bzImage；无镜像默认 SKIP，镜像存在后要求 QEMU 与 boot log 关键字符串通过 |
| `vmlinux_regress.sh` | pahole -J / objtool check vmlinux |
| `kernel_full_check.sh` | 编排 modpost + QEMU + vmlinux；strict 模式禁止核心门禁 SKIP，默认模式已有产物仍会执行对应门禁 |

### LTO 与 dead symbol 剔除

```bash
# 编译：-flto 默认开启 -ffunction-sections（.text.<func> 节）
dcc -flto -c -o a.o a.c
dcc -flto -c -o b.o b.c

# 链接：合并 .llvm.lto 摘要，ld --gc-sections 剔除无引用节
dcc -r -flto -o merged.o a.o b.o
```

**Windows 注意**：`*.sh` 必须为 LF 行尾。若在 Windows 编辑后 `pipefail: invalid option`，
在本地执行 `git ls-files --eol scripts/*.sh` 确认工作区为 `w/lf`，再重新保存。
Linux 环境下 LF 是默认, 不需要此步骤.

**prepare0 豁免**：~~`make prepare0 CC=dcc` 会触发 Kbuild 重建宿主工具（kconfig/conf 等），
dcc 宿主 C 尚未覆盖，所以只能用窄目标 + Rust fixdep 代过~~ —— 新目标下这条豁免要**关掉**：
宿主工具必须由 dcc 编（见「目标」节路线 1）。在它打通之前 `scripts/prepare0_full.sh` 仍是
可复用的窄路径，但它不再是验收口径。

**全树 smoke**：~~`make CC=dcc HOSTCC=gcc bzImage` 为当前推荐混合模式~~ —— **2026-11-01 改版后
这条不再成立**：编译步骤不许出现 gcc，链接也不许起 gcc 进程。内核 TU 本来就走 dcc（包装器已默认
不允许 gcc 回退，见 `DCC_KERNEL_ALLOW_GCC_FALLBACK`）；剩下的 gcc 通路都在「目标」节里点名：
宿主工具（`HOSTCC`）、链接驱动、`-m16` 那批 setup/实模式文件。验收目标也从「产出 bzImage」改成
「QEMU 跑到用户空间」（先走 PVH + `-kernel vmlinux`）。步骤 7/10 需先产出 vmlinux/bzImage。

**QEMU 启动验证**：`scripts/qemu_boot_matrix.sh` 使用 `qemu-system-x86_64` 加载
bzImage 并检测启动日志。内核必须启用串口控制台支持：

```bash
# 内核配置必须启用以下选项（defconfig 默认未启用）
CONFIG_SERIAL_8250=y
CONFIG_SERIAL_8250_CONSOLE=y
CONFIG_SERIAL_CORE=y
CONFIG_SERIAL_CORE_CONSOLE=y
```

可通过 `scripts/config` 启用：

```bash
cd /path/to/linux-7.1
scripts/config --enable CONFIG_SERIAL_8250
scripts/config --enable CONFIG_SERIAL_8250_CONSOLE
make olddefconfig
```

启动命令：

```bash
qemu-system-x86_64 \
    -kernel arch/x86/boot/bzImage \
    -m 512M -no-reboot -display none -nographic \
    -append "console=ttyS0 panic=-1"
```

预期输出包含 `Linux version 7.1.0` 等启动日志。
无 initramfs 时最终会因无法挂载根文件系统而 panic，这是预期行为，
`qemu_boot_matrix.sh` 会将此情况视为 boot smoke 通过。

**initramfs 启动验证**：`scripts/build_initramfs.sh` 构建最小 initramfs（busybox + /init 脚本），
内核成功启动后会尝试执行 init 进程：

```bash
# 构建 initramfs
bash scripts/build_initramfs.sh

# 启动带 initrd 的内核
qemu-system-x86_64 \
    -kernel arch/x86/boot/bzImage \
    -initrd initramfs/initramfs.cpio.gz \
    -m 512M -no-reboot -display none -serial stdio \
    -append "console=ttyS0 earlyprintk=serial,ttyS0,115200"
```

预期输出包含：
```
Run /init as init process
=== DCC kernel: init reached successfully ===
=== Boot to init complete ===
```

内核配置必须额外启用：
```bash
CONFIG_BLK_DEV_INITRD=y
CONFIG_RD_GZIP=y
CONFIG_DEVTMPFS=y
CONFIG_DEVTMPFS_MOUNT=y
```

**混合编译模式**：`scripts/dcc-kernel-cc.sh` 包装脚本实现 C 文件用 dcc、汇编文件用 gcc：

```bash
make CC=/path/to/dcc-kernel-cc.sh bzImage
```

这解决了复杂 GAS 宏（如 `ALT_INSTR`、`FILL_RETURN_BUFFER`）在 dcc 预处理后的兼容性问题。

### asm-offsets 验收（prepare0 步骤 4–5）

Linux 7.1 x86_64 在 `prepare0` 中会编译 `arch/x86/kernel/asm-offsets.c` 生成
`asm-offsets.s`（供后续提取结构体偏移常量）。

```bash
# 本地 (内核树 ~/linux-7.1, dcc ~/dcc/target/release/dcc)
# 手动：读取 .asm-offsets.s.cmd 中的 gcc 命令，将 gcc 替换为 dcc 后执行
cd ~/linux-7.1
dcc -Wp,-MMD,... -S -o arch/x86/kernel/asm-offsets.s arch/x86/kernel/asm-offsets.c
```

**2026-07-17 进展**：

| 指标 | 修复前 | 修复后 |
|------|--------|--------|
| 解析错误（E0xxx） | ~867 | **0** |
| 语义错误（E1xxx） | 6 | **0** |
| `asm-offsets.s` | 未生成 | **已生成**（~5 MB） |

**2026-07-16** 主要修复：语句表达式后缀调用、typeof(struct)、section 相邻字符串、
asm goto 试探解析、位域逗号分隔、结构体空字段、__auto_type、typedef 函数括号、
`__builtin_offsetof` 常量求值、object-like→function-like 宏链、多维数组字段、
for-init 枚举声明、属性后指针函数声明、GNU `?:` 省略中间操作数等。

**2026-07-17** 补充：include 缓存零参 function-like 宏（V6）、三元指针类型、
内联 asm GNU 局部数字标签（`6:`/`6f`/`6b`）、**`-O2` 保留 `goto` 目标 C 标签**、
**同行标签拆分**（`1:jmp` / `found: ret`）、**mini-asm 位测试/shld/shrd/bsf/popcnt/bswap/movnt**、
**GAS 伪指令**（`.pushsection`/`.balign`/`.long`/`.quad`）、**lock/rep 前缀**。

**modpost 宿主 C（2026-07-17）**：

| 修复项 | 说明 |
|--------|------|
| `NOFAIL` / `#` 字符串化 | 多行实参折叠为单行字面量，消除 `modpost.c:5560` Invalid token |
| `struct __attribute__((__may_alias__))` | 解析器完整支持 `may_alias` 类型属性：struct/union/enum 名前、体后、typedef 均可；存储于 `ParsedAttrs.may_alias`，禁用 TBAA，内核 `__bitwise` 类型 (`__le32`/`__be32`) 依赖此属性进行安全类型双关 |
| 兼容 typedef 重定义 | glibc/linux 头重复 `dev_t`/`uint64_t` 等 → `register_typedef` |
| 局部遮蔽 libc 名 | `index`/`basename` 等不再 E0007 |
| `typeof(_Generic(...))` | READ_ONCE 类宏；声明初始化 `sizeof(*v)` |
| asm `#` 行内注释 | `jump_label.h` `jmp %l[l_yes] # objtool NOPs this` |

**Windows 同步注意**：scp 后须 `perl -pi -e 's/\x0d//g'` 去 CR；勿用 `sed 's/\r//g'`。

**内核头文件**：`devicetable-offsets.c` 依赖 `include/generated/autoconf.h`。
若内核树仅有 `.config` 而无该头文件，同步脚本会自动执行
`make olddefconfig`（如需）与 `make include/generated/autoconf.h`（使用系统 gcc）。

## 1. 环境要求

### 1.1 硬件/系统

| 项目 | 最低要求 |
|------|---------|
| 架构 | x86_64 |
| 操作系统 | Linux (推荐 Ubuntu 22.04+ / Debian 12+) |
| 内存 | ≥ 2 GB |
| 磁盘 | ≥ 10 GB 可用空间 |

### 1.2 软件依赖

```bash
# 基础构建工具
sudo apt update
sudo apt install -y build-essential gcc make bc flex bison libssl-dev libelf-dev

# Rust 工具链 (用于编译 DCC)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Linux 内核源码 (7.1 版本)
cd /usr/src
sudo wget https://cdn.kernel.org/pub/linux/kernel/v7.x/linux-7.1.tar.xz
sudo tar xf linux-7.1.tar.xz
sudo ln -s linux-7.1 linux   # 方便引用
```

### 1.3 验证环境

```bash
gcc --version        # 确认 GCC 可用 (用于对比)
make --version       # 确认 make 可用
rustc --version      # 确认 Rust 可用
```

## 2. 编译 DCC

### 2.1 获取源码

```bash
cd ~
git clone <dcc-repo-url> dcc
cd dcc
```

### 2.2 构建

```bash
cargo build --release
```

构建产物位于 `target/release/dcc`。

### 2.3 安装到 PATH (可选)

```bash
sudo cp target/release/dcc /usr/local/bin/dcc
dcc --version   # 验证
```

## 3. 内核配置

### 3.1 使用默认配置

```bash
cd /usr/src/linux
make defconfig
```

### 3.2 调整配置 (推荐)

为了减少编译复杂度，建议关闭一些 DCC 尚未完全支持的特性：

```bash
# 使用 menuconfig 或直接编辑 .config
make menuconfig
```

建议关闭的选项：
- `CONFIG_LTO` (Link Time Optimization) — 需要特殊编译器支持
- `CONFIG_RETPOLINE` — 涉及复杂汇编
- `CONFIG_KASAN` — 内存调试，涉及内部编译器钩子

### 3.3 最小配置 (用于测试)

如果只想测试 DCC 能否编译单个内核文件，可以跳过完整配置，直接编译单个 `.c` 文件。

## 4. 单文件编译测试

### 4.1 预处理测试

验证 DCC 预处理器能否正确处理内核文件：

```bash
cd /usr/src/linux

# 预处理 init/main.c
dcc -E \
    -include include/linux/compiler-version.h \
    -include include/linux/kconfig.h \
    -nostdinc \
    -I include \
    -I arch/x86/include \
    init/main.c > /tmp/main_preprocessed.c 2>/dev/null

# 查看输出行数 (与 GCC 对比)
wc -l /tmp/main_preprocessed.c
gcc -E -include include/linux/compiler-version.h \
    -include include/linux/kconfig.h \
    -nostdinc -I include -I arch/x86/include \
    init/main.c 2>/dev/null | wc -l
```

DCC 的预处理输出应为 GCC 的 1.5-2.0 倍大小。

### 4.2 语法检查

使用 `-fsyntax-only` 模式测试解析器（不生成代码）：

```bash
# 预处理 + 解析
dcc -E \
    -include include/linux/compiler-version.h \
    -include include/linux/kconfig.h \
    -nostdinc -I include -I arch/x86/include \
    init/main.c 2>/dev/null | \
    dcc -fsyntax-only -

# 如果无输出则表示解析成功
echo "Exit code: $?"
```

### 4.3 完整编译 (生成 ELF)

```bash
# 预处理 + 解析 + 代码生成
dcc -E \
    -include include/linux/compiler-version.h \
    -include include/linux/kconfig.h \
    -nostdinc -I include -I arch/x86/include \
    init/main.c 2>/dev/null | \
    dcc -c -o /tmp/main.o -

# 检查输出文件
file /tmp/main.o
```

### 4.4 批量测试多个内核文件

```bash
#!/bin/bash
# test_kernel_files.sh - 测试 DCC 对多个内核文件的编译

DCC=/path/to/dcc
KERN=/usr/src/linux
cd $KERN

INCLUDE_FLAGS="-nostdinc -I include -I arch/x86/include"
FORCE_INCLUDES="-include include/linux/compiler-version.h -include include/linux/kconfig.h"

FILES="init/main.c kernel/panic.c kernel/sys_ni.c kernel/sched/core.c"

for f in $FILES; do
    echo -n "Testing $f ... "
    $DCC -E $FORCE_INCLUDES $INCLUDE_FLAGS "$f" 2>/dev/null | \
        $DCC -fsyntax-only - 2>/tmp/dcc_err
    
    if [ $? -eq 0 ]; then
        echo "PASS"
    else
        ERR_COUNT=$(grep -c 'error:' /tmp/dcc_err)
        echo "FAIL ($ERR_COUNT errors)"
        head -5 /tmp/dcc_err
    fi
done
```

## 5. 使用 make 集成编译

### 5.0 修完缺陷后如何接着上一轮进度跑（全树续跑标准流程）

用户定的做法：**不每轮从 0 重编**。改完 dcc 后的标准三步：

```bash
# 1) 重编二进制（不碰内核树）
cd /root/dcc && cargo build --release -j3
# 2) 只作废「不该再留着」的 .o：早于当前二进制的 + gcc 回退的；
#    宿主工具（scripts/、tools/）与 include/generated 全留下
cd /root/dcc && bash scripts/kernel_purge_stale_objects.sh /root/linux-7.1-dcc target/release/dcc
# 3) 同一棵树上续跑，失败/缺失的目标会被重做，其余直接复用
cd /root/linux-7.1-dcc && setsid env DCC_NO_PARALLEL=1 DCC_BIN=/root/dcc/target/release/dcc \
  make CC=/root/dcc/scripts/dcc-kernel-cc.sh \
       HOSTCC=/root/dcc/scripts/dcc-host-wrapper.sh -k -j2 >> /root/kwlogs/fullN.log 2>&1 &
```

> **2026-11-01 改版**：`HOSTCC=gcc` 这一行不再合法（目标要求全链不出现 gcc），改成
> `scripts/dcc-host-wrapper.sh`；并行度也从 `-j4` 收到 `-j2`（allmodconfig 在 7 GB 内存下会 OOM）。
> 包装器内部的链接步眼下仍会转给 gcc，那是路线 1 未完的部分，不是重新引入 gcc 编译的理由。

本轮实跑数字：`full7` 被停时树内有 920 个 `.o`，其中 909 个早于修完异常表那个缺陷的二进制
→ 只删这 909 个（外加 3 个 gcc 回退产物），剩下 14 个（宿主工具与 `realmode`/`boot` 那些有意
委托 gcc 的产物）全部留下；`make -j4` 从 0 个内核 `.o` 重新往上编，但省掉了宿主工具与
`asm-offsets`/generated 头那几十分钟。单目标验证一律写进 `/tmp`（`.work_tmp/kwreplay1.sh`、
`.work_tmp/kwbatch.sh`），不改树内产物；objtool 逐项扫用 `.work_tmp/kwobjtool.sh <对象>...`。

**步骤 1 与 3 不能颠倒，也不能只做第 3 步**：`cargo build`/`cargo test` 都会重写
`target/release/dcc`，而 Kbuild 只补「缺失」的目标、**不会重编已存在但属于旧二进制的 `.o`**
（它的 `if_changed` 只比命令行字符串，不认编译器版本）。实测踩到：改完东西没先 purge 就
`make -j4`，一直编到 877 个对象才人工发现这是一棵**混版树**（那样出来的 `vmlinux.o` 既说不清
是哪一版编的也不可复现）。所以：要么先杀 make 再改码，要么改完码一定先 purge 再 `make -k`。

下一轮若要改规则先验证再落地：`.work_tmp/kwbatch.sh <日志> <二进制> <并发> <目标>...`、
`.work_tmp/kwcheck_alt.sh <对象> 0,8`（逐条查 `.altinstructions`/`__jump_table` 的 orig 站点）、
`.work_tmp/kwcheck_endbr.sh`（与 gcc 参考树逐 TU 比 `endbr64` 条数）。

编译期诊断开关（都不参与任何判据，只打印）：`DCC_OPT_SKIP=<pass>` 跳过单个优化 pass（反向
验证杠杆）、`DCC_FOLD_TRACE=1` 打印常量 `switch` 折叠的退路类型与选择子形状、
`DCC_DUMP_LEFTOVER=1` 列出「内联后仍被直接调用」的强制内联函数、`DCC_DCE_TRACE=<fn>`、
`DCC_EMIT_TRACE=1`。注意 `DCC_SKIP_AI_BODY=1` 只能用于排障：它把「必须展开却展开不了」的
形状从可见的编译错误变成不可见的链接错误，不能当修复用。

### 5.0c 只影响一类对象时：按「缺陷签名」定向作废（不要清全树）

`kernel_purge_stale_objects.sh` 的判据是「.o 早于当前二进制」，它保不住几个小时的进度：
只要重编了二进制，**全部**内核 `.o` 都会作废。但很多修改只影响**能扫出签名的那一部分**对象 ——
此时应当先问「能不能从产物里把命中集判出来」，能就能只删那一小部分。

本轮实例（符号表同名重复登记，详 §5.4）：判据写在 `.work_tmp/kwduppurge.sh` 里 ——
扫全树 symtab，找「同一个名字既是 `FUNC`（size>0）又是 `NOTYPE LOCAL size=0`，且**节号或节内偏移不同**」
的对象。注意必须拿「节号/偏移不同」做判据：只看「同名有两条」会把同节同址的无害别名/标号也算进来，
实测从 528 误报压到 **209**（=真正会指错位置的那批）。执行：

```bash
bash .work_tmp/kwduppurge.sh              # dry-run，只列命中清单与数量
bash .work_tmp/kwduppurge.sh --delete     # 只删命中的 .o 与配对 .cmd 指纹
# 然后照 §5.0 第 3 步 make -k 续跑：只会重编命中的那批 + 本来就没编的
bash .work_tmp/kwduppurge.sh --verify     # 重编后复查，命中应为 0
```

这么做的两条前提，不满足就必须回到 §5.0 的清判据：① 改动在**未命中集**上不产生任何字节差异
（本轮是「只在已登记时少登记一条」，实测 21:52 全树 848 个现存对象 `--verify` 命中 0，含未删的 639 个）；
② 签名本身可以从产物里完整判定，不依赖只有编译器自己知道的事实。

另外两个本轮踩到的坑：① 回放器 `kwreplay1.sh` 缺省曾指向 `target/debug/dcc`，与全树用的 release
不是同一个二进制，验「修复有效」时产物逐字节相同会被误读成修改无效（现已缺省 release，要换请显式传第二个参数）；
② 删 `.cmd` 后 Kbuild 会连带重编同目录其他依赖生成头的对象（本轮 209 个删除引发了接近全树的一轮重编）——
这是代价，不是错误；真要控制它就不要删 `.cmd`，只删 `.o`。

### 5.0d 两棵树的身份必须拿证据判（删错树不会响，只会让后面的对照静默失效）

工作区里有两棵同构的内核树：`/root/linux-7.1-dcc`（**目标树**，必须全程 dcc）与
`/root/linux-7.1`（**参照树**，gcc 基线，用来逐 TU / 逐节 / 逐 objtool 判据对拍）。两边的
`.o` 外观同构，删错一棵不会有任何报错，只会让后面的 A/B 对照变成「参照侧莫名消失」。
因此：

1. 任何**写/删**内核产物的脚本，目标树一律显式传参，不靠缺省；
2. 身份判据用二进制级事实，不用目录名：dcc 树里每一条 `.*.o.cmd` 都写着
   `dcc-kernel-cc.sh`，参照树里一条也没有（`grep -l dcc-kernel-cc $(ls */.*.o.cmd | head)`）；
3. `kernel_purge_stale_objects.sh` 已内置这条守卫（找不到指纹就拒执）；新写同类脚本时请复用
   同一条判据，不要另建一套「按目录名猜」的规矩；
4. 参照树被误删后的恢复代价很小（gcc 全树 `make -k -j4` 约 4 分钟，实测 16:23→16:27），
   但要恢复到 **同一份产物**：重跑后核对 `vmlinux` 与 `arch/x86/boot/bzImage` 字节数与 mtime，
   再确认 `nm` 的基线符号集没变（本轮恢复到 1141 个 `.o` + 16,226,288 B `vmlinux`）。

### 5.0e 全树构建必须用**只读二进制快照**，否则实验会把树编成混版

本轮实踩：全树 make 与我的单 TU 实验共用 `target-next/release/dcc`，而实验要不断重编该
二进制 ⇒ 树里前后对象是**三个不同版本**编的（且 mtime 判据下全部作废，整树重来，白烧
约 35 分钟）。另有一个叠加的错：启动器 `echo $!` 记的是**外层 `bash -c`** 的 PID，真正的
make 在另一个 PGID 上，`kill -TERM -<错的pgid>` 只杀了包装进程 ⇒ make 本体照跑，杀了个假
「已停」。做法：

```bash
# 1) 选定当前二进制，装成只读快照（树只认这个路径）
install -m 0755 target-next/release/dcc .work_tmp/dcc-tree
md5sum .work_tmp/dcc-tree          # 记账：这一轮全树用的是哪个 md5

# 2) 按 §5.0 清一次（传快照路径作为基准二进制）
bash scripts/kernel_purge_stale_objects.sh /root/linux-7.1-dcc /root/dcc/.work_tmp/dcc-tree

# 3) 启动（runfull21.sh 自取真正 make 的 PGID 写进 pidfile）
bash .work_tmp/runfull21.sh

# 4) 之后实验随便重编 target-next/release/dcc，树不受影响；改完要入树时重跑 1)-3)
```

取 PGID 的正确写法（`ps` 的 `args` 字段**本身以命令名开头**，所以 `CC=/root/dcc/...` 在 `$5`
而不是 `$4`，按 `$4` 匹配会一条也匹不到）：

```bash
ps -eo pid,pgid,comm,args | awk '$3=="make" && $NF ~ /^CC=\/root\/dcc/ {print $2; exit}'
```

读巡控日志时也要区分两类红：`kill` 引发的 `*** … 已终止` 不是编译失败（实测 `full20.log`
11 条全部是这一类，零 E1xxx）；只有带 `错误 1`/`error[E1xxx]` 的才是真失败。

### 5.0f 只有链接器改动的修件**不必**清树（mtime 判据只看编译器）

`kernel_purge_stale_objects.sh` 的失效判据是「.o 的 mtime 早于 dcc 二进制」，它隐含的假设是
「二进制变了 ⇒ 产物可能变」。但改 `src/link/**`（内置链接器、resolve/relocate/layout）与改
诊断文本、`#[track_caller]` 日志这类，**不改变任何一个 `-c` 产物的字节**；而内核的
`vmlinux` 链接根本不走 dcc（包装器的 `link-step` 委托给宿主 `ld`）。所以：

* 纯链接器/纯诊断改动：**不需要** purge，也不需要重跑全树；直接拿现有对象继续；
* 判断方法（不要猜）：改完用一个新二进制重编几个已绿的 TU，与树上的产物 `cmp` 字节；
  全同且改动面只在链接器，就可以不重跑整棵树；
* 反之，只要动过 codegen/优化器（哪怕一行），就必须按 §5.0e 重装快照 + purge。

本轮实例：`src/link/resolve.rs` 把隐式未定义允许列表改成只对 `PIE/Shared` 生效（静态 ET_EXEC
不再把 `printf` 静默绑 0，见报告 §3.26）——正在跑的 `full24` 一行不重编，因为对象字节不受它影响。

### 5.0g 测试链必须单例：cargo 的锁会「等待」，而等待正好产出混版判据

同一台机器上同时开两条 `kwtestall.sh`（都是 `target-next`）会发生什么：第一条在跑
`cargo test --test integration`，第二条排在 cargo 自己的构建锁后面（日志里那句
“Blocking waiting for file lock on build directory”），等第一条编完它就立刻重编 —— 于是
**测试二进制里的 `dcc` lib 是 03:48 的源码，而 integration 用例源码是 04:14 才读进去的**。
这份混版判据直接给出 11 条红：10 条来自那个当时还在源码里、后来已回退的实验（实测证据：
拿 00:07 的快照二进制把这 6 个夹具在 -O0/-O1/-O2 上与 gcc 逐个对，全同），剩下 1 条是刚加的
新用例——它红得对（新 lib 还没编进去），但混在 10 条里就没法归因。

规矩：`kwtestall.sh` 现在自己拿 `target-next/.kwtestall.lock` 的 `flock -n`，**拿不到就退出**
（`exit 9`），不再排队等待。开链之前先看一眼有没有别的链：

```bash
pgrep -af 'bin/cargo' | head        # 注意 arg0 是全路径，^cargo 匹不到
ls -l --time-style=+%H:%M target-next/release/dcc target-next/release/deps/integration-*
```

二进制与用例源码的时间戳对不上（lib 早于本轮改动）就是混版，整轮结果作废重跑。与 §5.0e
是同一条纪律的两面：**编译器二进制与判据必须同一批源码**，只是那边管树、这边管测试。

### 5.0h `O=` 构建目录只认一个 CC：换 CC 跑任何 make 都会让全树作废（11-03 实测）

现象：为定位 modpost 段错误，用 `HOSTCC=gcc HOSTLD=ld make O=/root/pvh-lmc prepare0` 重编了
宿主工具；之后 `--build` 从 12:42 起把 2710 个 TU 从头重来，日志第 2~10 行是
`SYNC include/config/auto.conf.cmd` + `HOSTCC scripts/basic/fixdep` + `CC kernel/bounds.s`。

根因：kbuild 把 `$(CC) --version` 写进 `CONFIG_CC_VERSION_TEXT`，并据此派生
`CONFIG_CC_IS_CLANG` / `CONFIG_CC_IS_GCC` 与一大票 `-f…`/`-m…` 旗标。那次 make 没传 `CC`
（默认 gcc）⇒ syncconfig 把 `.config` 改写成 gcc 口径；下一次用 dcc 跑又改回来 ⇒
`include/config/auto.conf` 与 `include/generated/autoconf.h` 两次重生成 ⇒ 每个 `.o` 的隐含
依赖都过期。配置内容本身没被污染（改回后 `CONFIG_CC_VERSION_TEXT="dcc 0.1.0"`、
`CONFIG_CC_IS_CLANG=y` 都在），所以产物口径仍然一致，只是白重编一趟。

规矩：**排查宿主工具不要借 `O=` 目录**。要拿 gcc 版做对照，把 TU 拷到 `/tmp` 手工
`gcc -c` 再链到 `/tmp/xxx`，或另开一次性 `O=/tmp/xxx-build`（带独立 `.config`）。
判「本轮是不是增量」只看日志开头有没有 `SYNC`：有就是从 prepare 重来。

附带（磁盘）：`~/.dcc/cache` 一天能涨到 68 GB（78 283 个条目），196 GB 的盘在全量重编到
2550 CC 时只剩 2.5 GB。按 mtime 切掉「本轮起点之前」的条目不影响正确性（只是 miss 重算），
实测释放 47 GB：

```bash
cd ~/.dcc/cache && find . -maxdepth 1 -type f ! -newermt "2026-11-03 12:42" -delete
```

### 5.0i 语句表达式（`({ … })`）里的 `static` 必须登记成 static local（modpost 段错误的根因）

现场：`MODPOST vmlinux.symvers` rc=139；内核日志 `modpost: segfault at ffffffffffffffff`，
gdb 回溯 `internal_fnmatch(pattern=0xffffffffffffffff)` ← `match()` ← `secref_whitelist()`。
同一份 `vmlinux.o`（252 MB / 5399 节）换 gcc 编出来的 modpost 跑 rc=0 ⇒ 数据没问题，是
**dcc 把 modpost 编错了**。根因两处，都在语句表达式这条长尾上：

1. `src/static_locals.rs::collect_from_stmt` 只走 Block/If/While/Do/For/Switch/Label，
   **从不进入 `Expr::StmtExpr`**；而发射端 `Expr::StmtExpr` 分支本来就有 `is_static_local`
   判断，于是 `({ static const char *const patterns[] = {…, NULL}; patterns; })`
   （`scripts/mod/modpost.c` 的 `PATTERNS()` 宏）里的 static 表登记不上，退化成自动变量
   ⇒ 数组只剩首元素，`while ((pattern = *patterns++))` 一路读到栈上垃圾。
2. `codegen/x86_64/emit/expr/dispatch.rs` 的 `StmtExpr` 声明副本缺 `Expr::InitList` 分支
   （块路径 `gen_block_item` 有）⇒ `({ int v[] = {1,2,3}; … })` 只写首元素。

修：新增 `collect_from_expr` 复用 `opt::visit_expr_stmtexprs`（它的 `StmtExpr` 臂只回调
最外层 SE、不深入块内，嵌套 SE 由块遍历逐层发现，故不会重复登记），`collect_from_block`
对「非 static 声明的初始化式」也下探，`collect_from_stmt` 补齐
Return/Expr/If.cond/While.cond/Do.cond/For(init/cond/step)/Switch(expr+prelude)/GotoIndirect/
AsmGoto/Invoke/EhLandingPad。最小复现 `.work_tmp/{mp_repro,se_static,se_auto}.c` 在 -O2 下
与 gcc 逐项一致（`mp_repro` 三个模式全打印、SE 内 int 数组求和 10、char 数组求和 294）。

影响面（为什么必须全量重编）：全树 `({` 命中 13 696 处，`include/trace/stages/stage3_trace_output.h`
（`static const struct trace_print_flags __flags[]`）、`include/linux/once_lite.h`、
`workqueue.h`、`spinlock.h`、`gpio/driver.h`、`ring_buffer.h`、`drm_print.h` 都是这形状，
修前编出的对象都可能带着「static 退化成自动变量」的错，不能只重编 modpost。

### 5.0j 编译期谓词与语句表达式的折叠链（`vmlinux` 最终链接的四处必经修复）

现场：localmodconfig 全树 2730 个 CC 全绿、`MODPOST` 通过后，`LD .tmp_vmlinux1` 报一批
「只有声明、没有定义」的引用（这些函数是内核的编译期陷阱，gcc 靠「永远不发出引用」保证链得上）：

| 符号 | 条数 | 来源 |
|---|---|---|
| `___once_key` | 47 | `include/linux/once.h:50` `DO_ONCE()` 里语句表达式内的 `static DEFINE_STATIC_KEY_TRUE(___once_key)`，被 `__jump_table` 条目的 `"i"` 立即数引用 |
| `____wrong_branch_error` | 21 | `include/linux/jump_label.h:412/482/494` 的 `__builtin_types_compatible_p` 守卫 |
| `__field_overflow` / `__bad_mask` | 21 / 8 | `include/linux/bitfield.h:199/217` 的 `if (__builtin_constant_p(v) && (v & ~field_mask(field)))` |
| `__bad_udelay` | 3 | `include/asm-generic/delay.h:60` 同形 |

四处根因与落点（都在「常量折叠链」上，缺一条就整链断在此）：

1. **`const_eval::eval_stmt_expr_block` 不会跑直线程序**。它原先只认「表达式语句 + 带初值的声明」，
   一遇 `if` 就整体判非常量。而 `__always_inline` 助手搬进调用方后正是这形状：`field_multiplier()`
   体里那条 `if (…) __bad_mask();` 让 `field_mask(0xffff)` 求不出值 ⇒ 外层守卫折不成常量 ⇒
   陷阱调用真发进 `.text`。现在按位置解释块：`if` 只求被选中的那支；`goto` **只许前跳**到本块顶层
   标号且真的改执行位置（必须认：包装器把 callee 体的 `return E;` 发成 `rv = E; goto end;` + 末尾
   `end:` + 末表达式 `rv`，不认 `goto` 等于不认全部带提前返回的内联助手）；`x = E` 把值绑进环境；
   `return` / 循环 / `switch` / asm 一律拒绝（`({ … return E … })` 的 return 属于**外层函数**，
   拿 E 当语句表达式的值会造出错误常量）。上限 `INTERP_MAX_STEPS = 4096`：语句表达式嵌在折叠点里
   会被反复求值，没有上限就能把编译期推成平方级（本项目有过「按 trip 逐趟实体化导致挂起」的记录）。
2. **`opt::fold_expr` 的通用回填只在节点已是字面量时生效**。「原本不是字面量、但可求值」的节点
   （正是内联助手留下的语句表达式）永远折不掉 ⇒ 求值成功的语句表达式现在整体换成常量。这条的
   **前置条件**是第 1 条里的副作用闸门：赋值目标必须是本块自己声明的局部，否则替换会把一次写丢掉。
3. **`eval_asm_operand` 把地址常量打成源码名**。`"i"` 操作数拿到 `ConstValue::Addr { name }` 后
   直接 `name`，而函数内 `static` 在 symtab 里的键是 mangled 的 `.LslN.<func>.<x>` ⇒ `__jump_table`
   条目指着不存在的符号。现在过 `resolve_global_key`（这条规则在发射侧本来就有唯一入口，asm 立即数
   那一路原先漏了；`asm_memory_operand_str` 的 `"m"` 路一直是对的）。
4. **`__has_attribute(__fallthrough__)` 答 0**。见 §5.0k。
5. **链式 `else if (常量)` 不塌缩**。`fold_branches_block` 只能删「作为块内一项」的 `if`，
   而 `if (运行时) {…} else if (运行时) {…} else if (恒假) {死码} else {…}` 的第三支坐在
   外层 `if` 的 `else_` **操作数**上，以前永远删不掉。现场：`mm/rmap.o` 的
   `folio_referenced_one` 里 `else if (IS_ENABLED(CONFIG_TRANSPARENT_HUGEPAGE))` 在 THP 关时
   是死支，里面调的是 `include/linux/pgtable.h:546` 的 `BUILD_BUG()` 占位实现
   ⇒ `undefined reference to 'pmdp_clear_flush_young'`。新增 `collapse_const_if_operand`，
   安全性与块级折叠同口径（条件挂着未解析的 `__builtin_constant_p` 时不动；条件本身需要
   代码时原样留成一条语句）。同一机制还罩掉了 `drivers/md/dm.o` 的 `dax_get_private`/
   `dm_zone_endio`、`drivers/iommu/generic_pt/fmt/iommu_amdv1.o` 的 `__pt_no_sw_bit`、
   `net/mac80211/tx.o` 的 `__bad_mask` 与 `BUILD_BUG_ON`。
6. **`offsetof` 的下标不折叠**。`fold_expr` 没有 `Expr::Offsetof` 臂 ⇒
   `offsetof(struct __guc_ads_blob, engine_usage.engines[guc_class][ilog2(engine->logical_mask)])`
   里的 `ilog2()` 三元两条臂都发出来，产物同时引用 `__ilog2_u32` 与 `__ilog2_u64`
   （实测 `drivers/gpu/drm/i915/gt/uc/intel_guc_ads.o`）。
7. **`inline_expr` 同样不递进 `offsetof` 的下标**，所以折叠后露出的那个
   `static __always_inline` 仍没机会展开（`__always_inline` 是硬契约，gcc 在这种情况下
   直接报错）。除了补 `Offsetof` 臂递归，还在分支折叠不动点之后补跑一轮
   `function_inline(module, 0)` —— level 0 只做 always_inline/flatten，不带启发式，
   不会把代码尺寸再吹一遍。
8. **`N+sym` 形态的重定位名不剥位移**。per-CPU 成员访问给出的是与
   `sym+N` **反序**的写法（实测 `arch/x86/kernel/kvm.o` 报
   `undefined reference to '16+steal_time'`，而同一个对象里 `steal_time` 本身有定义）。
   既有设计是「位移在解析侧不拆、统一在 `split_sym_offset`/`reloc_sym_index` 剥」（
   `SymDisp(sym, _)` 有十来个发射点不看第二个字段，在解析侧拆名字会**静默丢偏移**），
   所以新分支加在同一个出口的最前面，并补一条 lib 用例钉住两种顺序与「不该动的形状」。

9. **块级 `const` 泄漏出块**。`var_quals`（`const` 赋值检查用的表）只按**名字**建、
   只在换函数时清，于是 `{ const unsigned int order = …; }` 出块后仍然遮蔽函数作用域
   那个可变的 `order`，后面的 `order = buddy_order(page);` 被误判成
   `E0030: assignment of read-only variable 'order'`（实测 `mm/compaction.c:643`；
   内核因为走 `-fpermissive` 所以降级成一条 W0030，不拦构建，但同一形状在不用
   permissive 的代码里就是直接拒编）。现在表里每条都带**登记时的作用域深度**，
   查表只认「深度 ≤ 当前深度」的最后一条 ⇒ 出块自然失效。为什么不用 push/pop
   标记栈：本解析器里块入口的 `push_scope()` 只有两处、出口的 `pop_scope()` 有四处，
   两个栈不配对，拿标记对齐会错位。用例
   `block_scoped_const_does_not_leak_out_of_its_block` 带两条反向对照（真 const 的
   直接赋值与复合赋值都必须仍然是错误）。

10. **具名 `struct`/`union` 的成员名等于一个可见 typedef 名时，布局错**（本目录里
    唯一一条**错编**级发现，不是警告级）。`parse_field_declarator` 里有一条「当前
    标识符看着像类型名 ⇒ 它属于下一个字段」的启发式（为匿名内联体+内核 `struct_group()`
    丢尾 `;` 那种状态写的），但它**不分匿名/具名**。于是 `struct mutex rl_t;` 被当成
    「没有声明符的声明」，成员丢名且偏移全错。最小可复现（`tests/fixtures/c/
    named_struct_field_named_like_typedef.c`）：`typedef void (*rl_t)(void*);` +
    `struct named_case { int head; struct mutex rl_t; int after_this; char tail[3]; int last; }`
    —— gcc 给 `size=20 head=0 rl=4 after=8 tail=12 last=16`，dcc 给
    `size=32 rl=0`（与 `head` **重叠**，后续字段全部后移）。真树现场：
    `include/sound/hdaudio.h:93` 的 `struct mutex regmap_lock;` 与
    `include/linux/regmap.h:257` 的 `typedef void (*regmap_lock)(void *)` 同名，于是
    `sound/hda/core/regmap.c` 里每一处 `guard(mutex)(&codec->regmap_lock)` 都报
    W1009（10 条，也是旧全树日志里唯一还没查过的诊断类）。修法：那条启发式只对
    **标签为空的匿名内联体**生效；具名结构体后面的标识符就是声明符（C 的成员与
    typedef 命名空间独立）。用例 `named_struct_field_may_share_a_typedef_name` 同时钉
    住反向对照（匿名内联体仍要被展平，`sizeof`/偏移不能变）。

实证：上面十条落地后，受害 TU（`mm/rmap.o`、`drivers/md/dm.o`、
`drivers/iommu/generic_pt/fmt/iommu_amdv1.o`、`arch/x86/kernel/alternative.o`、
`arch/x86/kernel/kvm.o`、`net/mac80211/tx.o`、`intel_guc_ads.o`、`sound/hda/core/regmap.o`）
用改道 `/tmp` 的真树重放
逐一核过，陷阱类未定义引用与 W1009 **全部清零**；新增集成用例
`bitfield_const_encode_folds_traps_away`、`once_key_asm_immediate_targets_mangled_static`、
`fallthrough_gnu_attr_double_underscore_is_recognized`、`pp_if_short_circuit_matches_gcc_undef`、
`has_attribute_accepts_reserved_identifier_spelling`、
`same_tu_empty_void_call_is_absorbed_but_not_over_absorbed`、
`dead_else_if_arm_is_collapsed_and_live_one_is_taken`、
`implicit_fallthrough_matches_gcc_shape_by_shape`、
`named_struct_field_may_share_a_typedef_name`（每条都带**反向对照**，防止「一律不折」
或「一律静音」蒙过去），lib 侧另有 `split_sym_offset_handles_both_operand_orders`、
`check_type_percent_d_ignores_signedness_but_not_float`、
`check_type_scanf_percent_s_compares_element_type`。
闸门：测试用例（lib）**2597 全过 / 0 失败**；测试用例（integration）在含前九条改动的
二进制上全量跑过 **1294 全过 / 0 失败**（debug profile，1121 s），含第十条（解析器）
的那轮正在跑；release profile 的完整复跑挨到本轮链接出结果之后一次做（不当换
`make` 正在使用的二进制）。测试用例脚本 `check_opt_equivalence.sh` 0 OPT-DIFF（四档行为一致）。

代价：这几条都改产物字节，所以按 §5.0f 的判据必须全量重编 —— 本轮就是 `kernel_purge_stale_objects.sh`
作废 2943 个 `.o` 后从 0 重跑的。

### 5.0k 警告要与 gcc 对齐，而不是靠 `-w` 静音

上一轮全树日志里 dcc 自身诊断共约 11 万条。判据不是「条数变少」，而是**与 gcc 同旗标逐条对齐**：
gcc 报的必须照报（那是内核自身的真问题或真缺陷），gcc 不报的才是 dcc 的噪声。

| 类别 | 修前全树 | 落点 | 现在的口径 |
|---|---|---|---|
| `warning[W0501]` implicit fallthrough | 44 185 | `is_known_attribute`（`src/preproc.rs`）与两处属性名比较没做 GCC 保留标识符归一化：`__has_attribute(__fallthrough__)` 答 0 ⇒ 内核整体退到 `#else` 支 `do {} while (0)  /* fallthrough */`，而那条只能靠注释识别 | 归一化放在唯一入口上；真树单 TU 现在 0 条 |
| `warning[W1015]` 内建返回类型未知 | 39 235 | `__builtin_unreachable` 37 964 + `__builtin_prefetch` 1 258 不在 C 侧已知内建名单（与 `__builtin_cpu_*` 同族：没有声明可写、后端按名字拦截） | 进名单；纯诊断改动，不改类型决定 |
| `unknown -m/-f option ignored` | 17 148 | 六个 clang 专属旗标 × 2858 TU（`CONFIG_CC_IS_CLANG=y` 下 `cc-option` 探测成功后必传） | 进 `is_known_m_option` / `is_known_f_option` 单点白名单，逐条写「对本后端无可观察语义」的理由；未知旗标照旧报 |
| `W0202` -Wundef | 4 093 | `#if` 求值器**不短路**：`replace_unknown_idents` 先把整行未知标识符换成 `0` 并逐个警告，再求值 ⇒ 永远读不到的那一侧也刷警告（`include/uapi/linux/stat.h:7` 2 536 条）| 顶层按 `,`→`?:`→`||`→`&&` 在最松处切并短路；单 TU 现在只剩 gcc 也报的那些裸标识符 |
| `W0202` 字节序那一族 | 本树余 656 | 上一轮把这条判成「gcc 同报，属真问题」，**那个结论是错的**。`include/uapi/sound/asound.h:561`、`include/uapi/linux/acct.h:114`、`include/uapi/linux/raid/md_p.h:163` 那族写的是 `#if defined(__BYTE_ORDER) ? __BYTE_ORDER == __BIG_ENDIAN : defined(__BIG_ENDIAN)`，而 `eval_lazy_top` 一见顶层 `?:` 就整行放弃切分 ⇒ 未参与运算的 `__BYTE_ORDER`/`__BIG_ENDIAN` 照样被警告。gcc 13 实测（`/tmp/gp/bo.c`，同旗标同输入）**0 条**；裸写法 `#if __BYTE_ORDER == __BIG_ENDIAN` 才报 2 条（`/tmp/gp/bo2.c`） | `?:` 也按最松算符切：先评 cond，再只评被选中的那一支（`:` 用 `?` 计数配对，GNU `C ?: D` 回落整行路径不改语义）。分支选择逐条钉住：`be_not_taken`/`one_taken`/`three_taken` 与 gcc -E 实测一致，用例 `pp_if_short_circuit_matches_gcc_undef` |
| `W0003` -Wformat 类型不匹配 | 2 550 | `format_check.rs` 里 `'d' | 'i'` 的可接受类型表有 `UShort`/`UChar`/`ULong`/`ULongLong` 却**漏了 `UInt`** ⇒ 内核满地的 `u32 & MASK` 喂给 `%d` 全被判不兼容（gcc 的 `-Wformat` 不看符号位，那是 `-Wformat-signedness` 的事） | 补 `Type::UInt`；真树 `drivers/usb/host/xhci-hub.o` 从 15 条到 **0 条**。用例 `check_type_percent_d_ignores_signedness_but_not_float` 带反向对照（`%d` 配 `double` 仍须报）。**已记账的另一侧松动**：宽度/秩差异现在一律放行（`%d` 配 `long long` 报不出来而 gcc 会报），收它得连 `spec.length` 一起判，不在本条里顺手改 |
| `W0010` `!__is_constexpr(…) is true` | 2 769 | C11 6.5.15p6 未实现（一侧指针、另一侧**空指针常量**时结果类型取那个指针类型）。内核的 `__is_constexpr(x)` 正是 `sizeof(int) == sizeof(*(8 ? ((void *)((long)(x) * 0l)) : (int *)8)))`（`include/linux/compiler.h:322`）：不实现时类型落在 `void *`，`sizeof(void)` 按 GNU 规则是 1 ⇒ `__is_constexpr` **永远为 0**，`__BF_FIELD_CHECK`/`const_true` 那族静态检查整片失效 | 取型与常量折叠**两处都要改**：条件也是字面量时 parser 先把三元折成被选中的那支，所以必须在折叠后补上按 p6 定出的类型（只对「指针 ⇄ 空指针常量」补类型标注，值仍是 0）。实测 `drivers/gpu/drm/i915/display/intel_crt.o` 从 14 条到 **0 条**。已记账的边缘：`(long)"字符串" * 0l` 仍不算空指针常量（gcc 算），内核不写这种形 |
| `warning[W0501]` implicit fallthrough（另两类假阳） | 本树余 3 条 | (a) `case A: case B:` 共享体的空 case：展开后本支体里只剩一条空语句占位，`last_effective_stmt` 返回 None 却落进「未标注贯穿」分支；(b) 以 `BUG();`（= `do { … __builtin_unreachable(); } while (0);`）收尾的 case：`do` 被当成「循环不算终止」 | gcc 实测（`/tmp/ft3.c` 四形）逐条对齐：空 case 不报、`case 1: int y = 2;` **仍报**（所以 None 跳过要额外要求「体内没有声明」）；`do{…}while(0)` 在条件恒假时看体内末条，`while(0)`/`for(;;)` 不进这一臂。用例 `implicit_fallthrough_matches_gcc_shape_by_shape`、`fallthrough_gnu_attr_double_underscore_is_recognized` |
| `W0003` scanf 侧的两类假阳 | 6 条（全树估计） | scanf 在查表前先把参数剥一层（`Ptr`/`Array` → 元素类型，`%d` 要的是「指向的那个对象」），但 `'s'`/`'n'` 两臂仍按 printf 的「指针/数组」判据比 ⇒ `sscanf(buf, "%20s %llu", tok, …)`（`char tok[21]`，`kernel/sched/core.c:10428`）与 `sscanf(p, "%lu%n", &tmp, &count)`（`mm/hugetlb.c:4285`）全被误报；另外可接受类型表还漏了 `Type::Array` 这一形 | scanf 下 `%s` 比**元素是否字符**、`%n` 比**元素是否 int**（gcc 实测：`%n` 配 `long *`/`char *` 都报，配 `int *` 不报）；printf 两条判据原样不动。用例 `check_type_scanf_percent_s_compares_element_type`；真树 `kernel/sched/core.o`、`kernel/cgroup/cgroup.o`、`mm/hugetlb.o` 重放后 W0003 归零 |
| `W0003` 格式属性的**数值目只认裸数字** | 本树 403 条（ACPICA 全家） | `include/acpi/platform/acgcc.h:31` 的 `ACPI_PRINTF_LIKE(c)` 展开成 `__format__(__printf__, 6, (6) + 1)`，而 `attr_uint_arg` 只读一个 Number token ⇒ `format_first_arg` 变成 6，恰好是**格式串自身的下标**，每个转换符都比错一格（实测 `drivers/acpi/acpica/dswstate.c` 19 条报的都是「`%X` 拿到 `const char *`」这种坑）| 属性目按**整型常量表达式**读：把本槽 token 文本收集到深度 0 的 `,`/`)` 再交给已有的纯算术折叠（与内联汇编位移同一工具，不开第二个算术子集）。实测三写法 `6, 7` / `6, 6+1` / `(6)+1` 现在给同一个答案；`drivers/acpi/acpica/dswstate.o` 从 19 条到 **0 条**，`security/selinux/selinuxfs.o` 从 12 到 0。用例 `format_attr_arguments_fold_constant_expressions` + 反向对照 `format_attr_arith_arg_still_catches_real_mismatch`（`%d` 吃 `char *` 仍须报，gcc 同报） |
| `W0010` `offsetof` 成员路径的下标被截掉 | 本树 2 条 | C11 6.7.9p6 的 member-designator 允许 `arr[常量]`，而 `eval_offsetof` 把 `[...]` **截掉不看** ⇒ 下标一律算成首元素偏移：`drivers/tty/serial/serial_core.c:3581` 的 `offsetof(struct serial_rs485, padding[1])` 给 12 而不是 16，两条布局 `_Static_assert` 假失败（顺带：顶层联合体以前直接返回 0）。gcc 同文件 0 条 | 逐段拆「字段名 + 若干下标」，按元素宽度前进（支持多维、下标里套括号），顶层联合体与结构体走同一条字段下降；读不出整型常量的下标返回 `NotConstant` 而不是默默折成一个偏小的数。实测 `drivers/tty/serial/serial_core.o` 从 2 条到 **0 条**。用例 `offsetof_member_path_accepts_array_subscripts` + 反向对照 `offsetof_subscript_mismatch_is_still_reported` |

| `W0003` 把 glibc 的 `%m` 当成吃实参的转换 | 本树 1 条（宿主工具） | `parse_format_specs` 的可接受转换表里带了 `m`，于是 `%m`（glibc printf 打 `strerror(errno)`，**不接实参**）也被计入「需要的参数数」⇒ `fatal("Can\'t open filenames file %s: %m", filename)` 这类写法每条刷一句凭空的「expects at least 2 argument(s), got 1」。gcc 同源码同旗标只对 `%d` 少参报、对 `%m` 不报（`/tmp/gp/mfmt.c` 实测） | `%m` 标成 `needs_arg=false`（既不计入 `needed`，也不推进 `arg_pos`）；scanf 侧的 `%ms`（`m` 是「库自己分配」的修饰符）仍解析成一个吃实参的 `%s`，`m` 留在 `length` 里以便诊断写法对得上源码。用例 `percent_m_directive_consumes_no_argument` 带反向对照（`%d` 少参必须仍报）；真树 `scripts/mod/modpost.o` 重放 1 → **0** 条，且产物**逐字节不变**（纯诊断改动，`cmp` 新旧二进制的同一 TU 已证） |

**本轮把 build12 里剩下的宿主工具警告逐条对齐完了**：`scripts/asn1_compiler.c:966/1487/1586`
那三条 W0501 是**真问题**（gcc 同源码同旗标在 `-O2` 下逐条报 `this statement may fall through`，
行号 967/1488 与我们的 966/1487 是同一个 case 支），不是假阳 —— 宿主工具不带 `-Werror`，
上游也带着它们。附带一条判据经验：gcc 的 `-Wimplicit-fallthrough` **要跑优化**才报，
所以拿 gcc 对拍时必须 `-c`（`-fsyntax-only` 一条都不报，会把真问题误判成假阳）；
`-O0` 也不报。而 dcc 是语法层判定，任何 `-O` 都报 —— 这一条差异记在这里，不打算改成
「跟着优化档走」，因为多报的那一侧报的都是同一条真问题。

遗留（已记账未修）：`-Wimplicit-fallthrough` 的**注释形态**（`/* fall through */`、`/* FALLTHRU */` 等
gcc `-Wimplicit-fallthrough=3` 的正则表）dcc 仍不认。内核绝大多数站点用的是 `fallthrough;` 宏，所以修完
`__has_attribute` 归一化后这类噪声已经不再来自内核；留着注释表是因为它需要预处理把「case 标签前的注释」
送到判据点，是另一套机制，不做「看起来差不多就吞掉」的事。
**本轮复核**：把 build9 里报 W0501 的 TU 逐个重放（`drivers/acpi/acpica/{exconvrt,psobject,rscalc,dswexec,dsmthdat}.c`、
`arch/x86/kernel/cpu/bugs.c`、`kernel/reboot.c`）现在全是 **0 条** —— 注释形态在本条修完后的 localmodconfig
配置下已经不可达（`__has_attribute(__fallthrough__)` 答 1 ⇒ 内核与 ACPICA 都走属性支），所以它留在账上
但不再是本轮的阻塞项。

判定装置：`scripts/kernel_replay_detached.sh <BUILD> <目标.o> [dcc]` —— 与批量体检用的
`kernel_replay_targets.sh` 的**关键差别**是把 `-o` 与 `-Wp,-MMD` 改道 `/tmp`。构建进行中必须用它：
`make` 正在写同名 `.o`，而重放只重放 `cmd_cc_o_c` 那一段、**不跑 objtool**，写回树里就等于把一个
没有 ORC 数据的 `.o` 混进 `vmlinux.o`。两处替换都得**前后带空格**，否则 argv[0] 与相邻 token 粘连成
`…/dcc-kernel-cc.sh.foo.o.d` 那种 `env: 没有那个文件或目录`(rc=127) 的假故障。

### 5.0l 尾声用 `mov %r13,%rsp` 恢复栈指针：objtool 跟不了（本树 46 491 条警告的来源）

build9 全树日志里 dcc 自身诊断只剩个位数，但 **objtool 侧刷了 46 491 条
`return with modified stack frame`**（另：9 715 条 `indirect call found in MITIGATION_RETPOLINE build`、
2 207 条 `redundant CLD`）。这不是噪声，是产物跟工具链的接口没对上：

1. **归因实验**（同一个自足片段，同一组旗标）：`gcc -O2 -mcmodel=kernel -fno-PIE
   -fomit-frame-pointer -fstack-protector-strong -fpatchable-function-entry=16,16
   -falign-functions=16` 产出的对象过 objtool **0 条**这类警告；dcc 产出的同一源文件对象
   也 **0 条**（那个函数走的是 `sub/add rsp` 对）；而真树 `drivers/tty/n_tty.o` 里几乎每个
   函数都命中的形状是：
   ```
   push %r13 ; mov %rsp,%r13 ; sub $0x390,%rsp      ← 序言：以 %r13 为帧基
   …
   mov %r13,%rsp ; pop %r13 ; jmp __x86_return_thunk  ← 尾声：从帧基恢复 rsp
   ```
   另两个旁证：gcc 把 16 字节 patchable 前缀放在**符号之前**（`inherit-0x10`），我们也是
   （`__pfx_n_tty_inherit_ops` 落在 nop 块首）⇒ 排除前缀布局不对；把 gcc 产物喂给**我们
   自己编出来的 objtool**也干净 ⇒ 排除宿主工具坏了。
2. **为什么 objtool 看不见**：`tools/objtool/arch/x86/decode.c` 是**白名单式**解码器，只给已知形状
   发 stack op（`push`/`pop`/`add·sub rsp,imm`/`leave` 等）；`mov reg,%rsp` 不在表里 ⇒ 不产生任何
   op ⇒ CFA 一直停在「帧已分配」的状态。到 `ret` 时 `has_modified_stack_frame()`
   （`check.c:2721`）比 `cfi->cfa.base`/`cfa.offset`/`stack_size` 与函数入口不相等 ⇒ 正是这一条警告。
   （同一函数里那 34 条 `unsupported stack register modification` 是 `check.c:3086` 那个分支：
   写法能解码、但目标寄存器就是当前 CFA base 的那些站点。）
3. **后果不是“少一条提示”而是 ORC 失真**：objtool 正是在这套 CFI 状态上生成
   `.orc_unwind/_ip`，跟不了栈状态的那条路径上发出去的 ORC 条目就是错的 ⇒ `arch_stack_walk`
   （lockdep、oops 转储、kexec）在这些函数上会给出错的回溯。不阻塞链接与启动，但它是本轮
   发现的**最大一类产物质量问题**，不靠 `-Wno-...` 静音（本树也没那个开关可用）。
4. **修法（本轮已落地）**：能证明平衡时把尾声的恢复改成 `add $frame_size,%rsp`（objtool
   能跟踪，也正是 gcc 在无帧指针模式下的做法），证不了就保留 `mov %r13,%rsp`（仍然正确，只是
   对本条警告保持现状）。落地形状是一张「本函数体内 rsp 比帧线低了多少」的账
   （`Codegen::rsp_below_frame_line`，`src/codegen/x86_64/instructions.rs`）：
   * 入口序言分配完帧（`emit/functions.rs:1138/1452` 的 `arm_rsp_ledger`）把账置 0，
     此后每一次发射都在唯一出口 `emit_insn` 里推进它；
   * 只有**能证明**的三种来源计入：类型化 `Stack{Push|Pop}`、类型化 `AluRm{Add|Sub, B64,
     dst=%rsp, src=Imm}`、以及 gas 文本里「最后一个逗号之后是 `%rsp`」的 `sub`/`add`；
     其它写 rsp 的形状（`mov %r13,%rsp`、`lea …,%rsp`、`sub %r11,%rsp`、`and $-16,%rsp`、
     `leave`/`enter`/`int`/`iret`）一律**弃账**，弃了就不再捡回来；
   * 内联汇编在语句级弃账（`mark_asm_stack_effects`）：模板是原样转发的，逐行解码等于再养
     一个解码器，所以 `push`/`pop`/`leave`/`enter`/`int`/`iret` 与把 `%rsp` 当**寄存器**用的
     都打掉整函数；`(%rsp)` 这种「只当内存基址」的不算（内核 `ALTERNATIVE` 的
     `lock addl $0,-4(%rsp)` 就是这一形，误判会把大量函数打进旧式尾声）。
     该判据带 lib 用例 `rsp_mentioned_as_register_is_not_a_memory_base`（含反向对照）；
   * 线性累加唯一的盲区是「控制流跨过窗口」，所以另加两道守卫：**账非 0 时定义标号**与
     **账非 0 时发跳转**都当场弃账；
   * `cur_func_do_align`（过对齐帧）、frame-pointer 模式（序言多推的 8 不在 `frame_size` 里）、
     VLA（`subq %rax,%rsp` 那种动态下推）与 naked 都继续走旧形式。

实测（重放 `kernel/sched/core.o`，objtool 用 kbuild 同一条命令行）：
`return with modified stack frame` **317 → 27**，对象总警告 489 → 197；尾声形状 562 条走
`add $帧长,%rsp`、67 条仍是旧式（弃账原因：条件跳转落在窗口里 24、模板把 `%rsp` 当寄存器 7、
标号处账非 0 的 3、`lea …,%rsp` 1）。剩下 27 条是那些**证不了**的函数，不猜。

落地中踩到的一个真回归（值得记）：尾声里我最初是「恢复落地后重新开账」，于是 VLA 函数
弃账之后的**第二个**返回点又敢发 `add $帧长,%rsp` —— 实测
`tests/fixtures/c/va_arg/vla_agg_via_va_arg.c` 在 -O0 直接 SIGSEGV（`va_arg_vla_agg_via_va_arg_matches_gcc_at_all_levels`
抓到，四档现与 gcc 同为 0）。正确做法是把尾声期间暂停的那本账**原样放回**：
`stack_shift = 0` 那一招只对「mod 16 同余」成立（VLA 下推是 16 对齐的），对「绝对等于零」不成立，
不能照抄。
   为什么不选「把帧基从 %r13 换成 %rbp + `leave`」：那一条确实免掉平衡分析（`leave` 在解码器
   白名单里，`check.c:2755` 的 `has_valid_stack_frame` 也专门给 CFI_BP 开了路径），但帧基编号
   渗在后端每一处寻址里（`emit_xmm_r13_disp`、`abi/layout.rs::frame_base`、`blockopt.rs` 的
   `keep_frame_base`、跳表还额外占用了 %rbp），爆炸半径比一个尾声大得多。

验证口径（修之前定好，免得到时候拿嘴说）：拿 `scripts/kernel_replay_detached.sh` 重放同一组
TU，用树上那个 objtool 按 kbuild 完全相同的命令行扫（`--hacks=jump_label --hacks=noinstr
--hacks=skylake --orc --retpoline --rethunk --static-call --uaccess --prefix=16`），比修前/修后的
`return with modified stack frame` 条数；同时三道闸门（lib / integration / `check_opt_equivalence.sh`）
必须全绿 —— 本条改的是产物字节，不改字节的那条「只能靠静音消除」的路不走。

### 5.0m `-mretpoline-external-thunk` 曾被当成「无语义旗标」收下：间接分支没走 thunk（本轮已修）

上一轮把六个 clang 专属旗标送进 `is_known_m_option` 白名单时，给
`-mretpoline` / `-mretpoline-external-thunk` / `-mindirect-branch-cs-prefix` 写的理由是
「只影响 clang 自己生成间接跳转蹦床的形状，retpoline 的实际代码由内核头文件里的宏完成」
（`src/cli.rs:987`）—— **这句是错的**，本轮现场：

1. C 层面的间接调用/跳转是**编译器**的活。头文件里的 `CALL_NOSPEC`/`JMP_NOSPEC` 只管内联汇编那
   一处；`arch/x86/Makefile:326` 的 `checkbin` 直接拿 `RETPOLINE_CFLAGS` 非空当「编译器会做
   retpoline」的判据，而 `cc-option` 探测在我们包装脚本上是**无条件成功**的 ⇒ 检查被骗过去了。
2. 产物现场：重放 `kernel/sched/core.o` 里有 130 条裸 `call *%r11` / `jmp *%reg`，
   objtool 因此报 `indirect call found in MITIGATION_RETPOLINE build` 全树 9 715 条。
3. 内核期望的形状（`arch/x86/lib/retpoline.S` + `arch/x86/kernel/alternative.c:879/915/1350`）：
   `call __x86_indirect_thunk_<reg>`（`-mindirect-branch-cs-prefix` 还要给它套上 `2e` 前缀），
   符号名就是 `__x86_indirect_thunk_r11` 这一族；objtool 的 `--retpoline` 靠
   `arch_is_retpoline(sym)` 认出这些调用并生成 `.retpoline_sites`，启动时 `apply_retpolines()`
   再按站点改写。换句话说：**不实现就只能裸看间接分支，而且没人会报错。**
4. 本端已有的部分：`target.indirect_branch_thunk_extern`（`abi/call.rs:1271`）会发
   `call __x86_indirect_call_thunk_r11`——**名字与内核不一致**（多了 `_call_`），而且只有
   `-mindirect-branch=thunk-extern` 那一条能把它打开（`cli.rs:2720`），内核传的 clang 形旗标
   走不到。间接**跳转**（switch 跳表 `emit/stmt.rs:238/1547`、`GotoIndirect`）一条也没转。

**本轮落地的修法**（`Codegen::indirect_thunk_sym` 是唯一判据点）：

| 落点 | 内容 |
|---|---|
| `src/cli.rs` | `-mretpoline-external-thunk` 从「对本后端无可观察语义」那张表里**拿出来**，接上 `indirect_branch_thunk_extern`（与 gcc 的 `-mindirect-branch=thunk-extern` 同一个开关）。`-mindirect-branch-cs-prefix` 仍留在表里，但理由换成实测的那条：objtool 的 `arch_is_retpoline`（`tools/objtool/arch/x86/decode.c:912`）按**符号名前缀**认站点，内核 `patch_retpoline()` 也是从调用位移解出蹦床，两处都不看 `2e` 前缀 |
| `abi/call.rs:1271` | 符号名改成内核真实存在的 `__x86_indirect_thunk_r11`；原来发的 `__x86_indirect_call_thunk_r11` 只在 `CONFIG_MITIGATION_CALL_DEPTH_TRACKING=y` 才存在，本配置下那是条未定义引用 |
| `emit/stmt.rs` | `goto *ptr` 与跳表分派的寄存器间接跳转同走蹦床；两条**内存**间接跳转的 SIB 快车道（`jmp *sym(,%rcx,8)` / `jmp *(%rbp,%rcx,8)`）在蹦床模式下让路 —— 内核的 retpoline 只覆盖寄存器形（gcc 的 `-mindirect-branch-register` 就是为它存在的），先读表项进寄存器再走蹦床 |
| `special/builtins.rs` | 两处编译器自发的间接调用（r11 / r10）同走蹦床 |

实测（重放 `kernel/sched/core.o` + 树上 objtool 按 kbuild 同一条命令行）：`call *`/`jmp *`
从 107 条降到 **0**，`indirect call found in MITIGATION_RETPOLINE build` 归零，`nm -u` 里出现
`__x86_indirect_thunk_r11`，对象总警告 489 → 115。用例
`retpoline_external_thunk_routes_indirect_branches_to_kernel_thunks`（正向：引用蹦床符号且产物里
不留裸间接分支）与 `indirect_branch_without_retpoline_flag_stays_plain`（反向对照：不开旗标时
**不得**无端引用蹦床符号，否则普通程序会多一个未定义引用）。

仍未修（诚实记账）：`-mretpoline` 的**内联** retpoline 序列（内核只给 vDSO 传这一形，
`arch/x86/Makefile:21`）—— vDSO 是独立链接的共享对象，拿不到 `retpoline.S` 里的外部蹦床，
既不能按外联形发（会留未定义符号），也不能当已实现。vDSO 那几个 TU 里若出现 C 层间接分支，
就是这条未修的现场。另：`redundant CLD`（core.o 88 条、全树 2 207 条）本轮从 62 涨到 88，
方向可解释：间接调用变成对**已知符号**的直接调用后，objtool 能看穿被调体的 `DF` 效果，
于是把我们那批 `cld` 判成冗余 —— 真问题是「我们在字符串操作前发了 gcc 不发的 `cld`」，
记在账上另行处理。**这条现在有基线了**：`/root/linux-7.1`（gcc 编的参照树）前 40 个 `.o` 里
 `cld` 是 **0** 条，而 `build12` 头 40 个 `.o` 里是 126 条 —— 即这些 `cld` 全是我们自己发的
 （`emit/expr/{float,complex,call,lvalue,dispatch}.rs` 里 25 个 `NoArgOp::Cld` 发射点，
  全后端从不发 `std`）。按 x86-64 SysV「进入与离开时 DF=0」的约定它们确实多余，gcc 就直接
  `rep movsq` 不带 `cld`；去掉能同时消掉这批警告与那批指令。做法上不要一刀切：只有当本函数
  的**内联汇编模板里出现 `std`** 时才保留 `cld`（那才是唯一能把 DF 弄脏的来源），判据与
  `mark_asm_stack_effects` 的语句级扫描同源。本轮不动它 —— 它是「多发一条指令」，不是错码，
  而改产物字节要再换一次全量重编，先把链路与启动打通要紧。

**操作事故一条（别再犯）**：`tools/objtool/objtool`、`scripts/basic/fixdep` 这类宿主**二进制**
不是 per-object 规则的前置（`Makefile.build:548` 那条 `cmd_objtool` 只是 `./tools/objtool/objtool`
直接执行），所以**构建进行中删掉它们**会把整棵树立刻打成
`/bin/sh: 1: ./tools/objtool/objtool: not found` + `错误 2`；而从 `O=` 目录直接
`make tools/objtool/objtool` 也没有规则。要作废宿主工具就得**整轮重跑**：先删干净 `.o`，
再从 `vmlinux` 这条正常依赖链让它自己重建（本轮实测重建出的 objtool/fixdep 时间与二进制同源）。

### 5.0n 待查：同一棵 `O=` 树上并发跑第二次 `make olddefconfig` 会刷 `Aborted (core dumped)`

现场（本轮）：`build12` 正在同一棵 `/root/pvh-lmc` 上跑 `vmlinux` 时，另开一次
`make -s O=/root/pvh-lmc ... olddefconfig`，会看到 1~3 条 `Aborted (core dumped)`
（**条数每次不同**），但 `rc=0` 且生成的 `.config` 与快照**逐字节相同**；把同样的命令搬到
独立的 `O=/tmp/pvhcfg` 里从零跑（宿主 kconfig 工具全部现编），**一条 abort 都没有**，
产物 `.config` 与主树那份 `diff` 为空。已排除的嫌疑：`dcc` 对「源文件不存在 / 是目录 /
输出目录不存在」三种 I/O 都发干净诊断（`rc=1`）不 panic；kconfig 的
`cc-option` 形状 `$(CC) -Werror … -E -x c /dev/null -o /dev/null` 单独喂也正常退出。

所以最像是「两次 make 在同一 O 目录里抢同一份临时状态」（`.tmp_*` / `include/config/*`）
的产物。为什么屏幕上只剩一句 `Aborted (core dumped)`：kconfig 的 `cc-option` 那一族都走
`try-run`，形状是 `$(cmd) >/dev/null 2>&1`，被 probe 的进程 abort 时，**它自己写在 stderr 上的
panic 文本连同 stdout 一起被丢掉**，剩下的那句 `Aborted (core dumped)` 是父 shell 报的。
也就是说「装个 panic 钩子让崩溃可见」这条不需要做（`panic = "abort"` 下默认钩子已经会先把
`thread '…' panicked at …` 写到 stderr，只是被 `2>&1 >/dev/null` 吞了）；要做的是**定位那一次
panic 的输入**。

**已复现并已排除的范围**（别重做）：把同一棵 `O=` 树换成独立的 `/tmp/pvhcfg`，只要
**并发跑三次** `make -s O=/tmp/pvhcfg … olddefconfig`，abort 就稳定出现（那次抓到 4 条），
所以跟正在跑的 `vmlinux` 构建无关，纯粹是「同一份 O 目录里的并发 kconfig」自己抢出来的。
`strace -f -e trace=process` 抓到的链条是：某个 `sh` 在 `wait4` 收完子进程之后
`execve("/root/dcc/target/release/dcc", ["--version"])` → 它的工作线程（`clone3`）收到
`SIGPIPE`（因为同时另一路的 stdout 端已关闭）→ 紧接着 `tgkill(SIGABRT)`。
另已单独排除：`dcc --version`/`--help` 单发与 20 路并发都正常；`dcc -E 大文件` 的
stdout 管道被提前关掉时，我们发的是干净诊断
`error[E0101]: write error: Broken pipe (os error 32)`、`rc=1`（debug 与 release 同），
不是 panic —— 也就是说「写闭管」这条路径本身是好的，abort 出在 `--version` 那次调用的
**并发上下文**里（stderr 被 `try-run` 丢进 /dev/null，所以那句 panic 文本看不到）。

本轮不当结论用：验收判据看的是**配置结果**（已证与快照、与独立目录从零生成三者一致）。
待 `build12` 跑完、树上没有并发 make 时再复跑一次那条命令：若 abort 消失 ⇒ 记为
「构建进行中禁止再跑 `--config`/`olddefconfig`」的操作纪律；若仍在 ⇒ 上 `strace -f -e
trace=process` 抓 `SIGABRT` 的那个 pid 的 `execve` 行，把当时的完整命令行捞出来（再把它的
stderr 从重定向里解放出来单独复现），按缺陷处理。

### 5.0o 阻塞链接的两处「编译期陷阱函数」残留（已修，修法与定案见 §5.0p）

对 build12 已产出的 1358 个对象做 `nm -u` 全树扫描，抓到 7 个对象还引用着**全树没有定义**的
编译期陷阱函数（`grep -rn` 全树 + 已产出对象里都查不到定义 ⇒ 这两条一定会把最终链接打成
undefined reference）：

| 符号 | 对象（各 1 条引用） | 现场 |
|---|---|---|
| `____wrong_branch_error` | `net/ethtool/ioctl.o`、`net/netfilter/{nf_conntrack_netlink,nf_conntrack_expect,nf_conntrack_core,nf_nat_core}.o`、`drivers/tty/serial/8250/8250_platform.o` | `include/linux/jump_label.h:412/482/494`：`static_key_enabled` / `static_branch_*` 那三个 `__builtin_types_compatible_p` 守卫的 `else` 支。取支是 `arch_static_branch*(…)`（`asm goto` + `%l[l_yes]`），所以塌缩要连着标签一起处理 |
| `__pt_no_sw_bit` | `drivers/iommu/generic_pt/fmt/iommu_amdv1.o` | `pt_fmt_defaults.h:252-263`（该格式没有软件 bit 的那一支）：`pt_test_sw_bit_acquire()` 体里无条件调它；真正引用它的是 `iommu_pt.h:667` 的 `pts_feature(&pts, PT_FEAT_DMA_INCOHERENT) && !pt_test_sw_bit_acquire(...)`（`readelf -r` 抓到 `R_X86_64_PC32 → pt_test_sw_bit_acquire - 4`，在 `__map_range` 里） |

两条已排除的解释，别再走一遍：
1. **不是「死代码没删」那么简单**。`net/ethtool/ioctl.o` 里那段陷阱块前面是一条 `jmp acf2`，
   跳转来自同一个 `static_branch_likely` 克隆（`__inl12090`/`__inl12091` 两份）的 `asm goto`
   标签区 —— 也就是**标签目标被接到了本该塌掉的另一支上**，那段代码在产物里是「可达」的。
2. **不是内联策略漏了**。手写最小件（`static inline` 体里无条件调陷阱函数，调用点被常量
   `if` 挡掉）在 -O2 下我们和 gcc 一样不发引用；差异只在 -O1 一档（我们留、gcc 不留），
   不是本条现场。真正的形状需要带 `asm goto` 的取支 + 类型守卫的链式 `else if`，
   我按 `wb3/wb4/wb5/v3/v5/wb6` 六版最小件都还没复现出来 —— 下一步该从编译器自己的
   视图切进去（`DCC_DUMP_FN` 在这个 TU 上没输出，先把 dump 入口对上），而不是继续猜形状。

**复现与定位进展（本轮）**
1. 先记一条**我自己踩的错**：`/tmp/gp/feat.c` 那版「最小件」是无效的 —— 里面 `call_b` 用运行时
   下标 `n` 调 `feat()`，`trap()` 是**合法可达**的引用；我按整个 `.o` 的 `nm -u` 计数把它当成了
   缺陷。按函数拆开后（只留 `call_a`）-O2 折得干干净净。⇒ 计数必须按函数拆，不能看 TU 总数。
   据此写的一次改动（拼接时「体内只有一条尾 `return`」就不发出口标签）在 7 个真对象上
   **一条都没改善**，已回退，不留投机改动。
2. 拿到了**自足的真复现件**：`dcc -E` 把 `drivers/iommu/generic_pt/fmt/iommu_amdv1.o` 的命令行
   原样改 `-E` 得到 `/tmp/gp/amdv1.i`（70117 行，无头文件依赖），直接
   `dcc -O2 -ffreestanding -c amdv1.i` 就复现同一条 `__pt_no_sw_bit` 引用。
3. 用 `DCC_DUMP_FN=__map_range DCC_DUMP_PASS=eliminate_unreachable` 看到折叠**停在哪一格**：
   `pt_feature()` 的两道守卫已被折成 `Int 196`/`Int 246`，但条件树停在
   `Binop{And, Int 196, …}`（另一侧还是 `1UL << 形参临时量`）；再往外是
   `If { cond: Ident("__inl9084$rv") }`，里面裹着 `Call(__pt_no_sw_bit)`。
   ⇒ 缺口是**「形参临时量被赋常量后代回条件」这一步在宿主函数里没有发生**。再往下一层，
   `DCC_FWD_TRACE=1` 把范围收窄到「位置轴」：`propagate_const_stores_forward` 的候选表里
   `__inl9084$_feature_nr` 与 `__inl9084$rv` **都在**（`hazard=[]`，名字轴没被否决），
   所以丢的是位置轴 —— 存储与读取之间隔了「有 `goto` 指向」的标号，按该 pass 的既有规则
   （那种标号必须清表，否则 `goto` 绕开存储的路径会被误代回）当场作废。
   ⇒ 修法要么在**展开时**就把常量实参直接写进被展开的体（那时还没有标号可言），要么把清表
   判据细分（只挡真正跨过该存储的后向/前向入边，而不是一律清表）。两条都要以 `amdv1.i` 与
   `net/ethtool/ioctl.i` 为验收件，并同时看 `general_inline_*`、`cprop_*` 那批既有用例不许退化。
4. **靶子已缩到 34 行**（`.work_tmp/repro_swbit_trap.c`；修好后按惯例转正为带反向对照的用例）。
   实测：`gcc -O2` 不引用、`gcc -O0` 引用（合法形状）；`dcc -O1/-O2` **都引用**。
   `readelf -r` 还抓到 `map_one` 里一条 `R_X86_64_PC32 → test_bit - 4` 的真调用，`nm` 里
   `test_bit` 以本地 `t` 符号实体化 ⇒ **上一条「位置轴/合成标签清表」的归因是错的，作废**。
   真实形状：`A && !test_bit(p,1)` 里的 `A` 被展开成「被调体的块 + 尾部结果临时量
   `__inlexp1$rv`」，折叠层见 `A` 是个 `Ident` 就当不可知 ⇒ `&&` 不短路 ⇒ 右侧那次
   `test_bit` 调用留在产物里；而那个块里的两道守卫其实早已折成常数（同一函数体在
   `DCC_DUMP_PASS` 里从 118K 字符缩到 84K、`__inl` 临时量 22→12，都是折叠确实发生过的证据）。
   再剥一层，缺陷的**最小本质只有 9 行**（`.work_tmp/repro_nested_inline_constfold.c`）：
   ```c
   extern void trapv(void);
   static inline int inner(int c) { if (c) return 1; return 0; }
   static inline int outer(int c) { return inner(c); }
   int three(int *p) { if (outer(0)) trapv(); return 1; }
   ```
   `gcc -O2` 不引用 `trapv`，`dcc -O2` 引用；对照件「单层 `static inline`」（`if (feat(0)) trapv();`）
   我们能折（0 引用）⇒ 缺的是**「拼接出来的块还要再折一次、直到定点」**：`outer` 在定义处不是
   「一条常量 return」，`fold_const_return_calls` 认不出它；拼接后 `outer(0)` 成为「块 + 尾部结果
   临时量 `__inlexp1$rv`」，块里 `inner(0)` 已能折成 0，但没有哪一步把「该块的常量结果」递回
   外层 `if` ⇒ 不短路 ⇒ 被调侧那次调用连同实体化的本地副本一起进产物（`readelf -r` 里
   `R_X86_64_PC32 → test_bit - 4`）。修法两步：① 对拼接出的块走一次
   `const_eval::eval_stmt_expr_block`（它已能跑直线块 + 局部量），出常量就整体替换；
   ② `fold_const_return_calls` 在拼接**之后**再收一轮（先把 `outer` 的体折成 `return 0;`）。
   同族还有 `____wrong_branch_error` 那 6 处。验收：`m3`/`t4` 两件对 `gcc -O0`/`-O2` 的差分
   + `nm -u` 全树复扫那 7 个对象 + 三道闸门。
4b. **复现件再降一层：1 行纯 C**（`.work_tmp/repro_cprop_across_label.c`，另附对照三形
   `.work_tmp/repro_cprop_shapes.c`）：
   ```c
   int s10(void){ int r1,r2; if(0){r2=1;}else{r2=0;} r1=r2; if(r1) trapv(); return 1; } /* 折得动 */
   int s11(void){ int r1,r2; if(0){r2=1;goto e;}else{r2=0;goto e;} e: ; r1=r2; if(r1) trapv(); return 1; } /* ✗ */
   int s12(void){ int r1; r1=({int r2;r2=0;r2;}); if(r1) trapv(); return 1; }             /* 折得动 */
   ```
   `gcc -O2` 三形都不引用 `trapv`；`dcc -O2` 只有 **s11 引用** ⇒ 唯一差别就是那个
   **「有 `goto` 指向的标号」把 cprop 的位置轴切断了**（死支里带 `goto`，所以折叠后
   `r2 = 0` 仍留在臂里，跨标号那次 `r1 = r2` 拷贝没被代回）。`DCC_FWD_TRACE=1` 的现场：
   `store name=r2 c=0 in_qual=true moved=false` + `adjacent label=e known=1 edges=1`
   （紧邻规则**认了**这块标号、表没清），可是随后就没有 `store name=r1` 了 ⇒ 缺的是
   「跨过紧邻标号之后，本项的读点也要按表代回」这一步（`forward_items_with`，
   `src/opt.rs:5240` 起：紧邻标号那支只保住了表，没把代回继续喂给后面那一项）。
   内核里 `__pt_no_sw_bit`/`____wrong_branch_error` 这 7 个对象就是这个形状（桩被拼接成
   「臂里存结果临时量 + 块尾 `goto` 出口标签」，见 `.work_tmp/repro_nested_inline_constfold.c`）。
   验收：s11 折成 0 引用、s10/s12 不退化 + `nm -u` 全树复扫那 7 个对象 + 三道闸门。
   已排除的两条假路（别再走）：① 把 `fold_const_return_calls` 多跑几轮（无效，已回退）；
   ② 把这条不动点的 `for _ in 0..3` 提到 6（无效，已回退）。

5. **一条自我纠正**：本轮我在汇报里贴过一段自称读自 `src/codegen/aarch64/` 的
   `prop_scalar_imm` 代码，仓库里**没有这个函数**（`grep -rn prop_scalar_imm src/` 为空）——
   那是把没发生过的读取当成了证据，相关的「aarch64 待办」并不存在，撤销。教训：引用代码
   必须当场 `grep`/`Read` 命中过，不能凭上一轮印象。

影响与优先级：**这两条会让 `LD vmlinux.o` 之后的最终链接直接失败**，属于「重新开始」这条链上
必须清掉的缺陷，不是可记账绕过的质量问题。build12 继续跑完仍有价值（后半棵树的诊断普查），
但它的链接阶段注定在这两个符号上失败 —— 修好之后连同 cld/`%m`/尾声硬化一起出最终那一发。

### 5.0p 陷阱符号归零：三处定案与一处误判的纠正（本轮）

全树 `nm -u` 扫到 18 个对象引用「只有声明、没有定义」的编译期陷阱（`____wrong_branch_error`
17 个 + `__pt_no_sw_bit` 1 个）。逐个重放到 /tmp（`scripts/trap_symbol_replay.sh`，只读
`.cmd` 里的 `savedcmd_…`、把 `-o`/`-Wp,-MMD` 改到临时目录，构建树一个字节不动）后的结论：
**18 个全部折成 0 引用**，与 `gcc -O2` 逐对象对齐。这条路上一共卡在三格，各自都能单独钉住
整条折叠，所以必须三格一起改才算修完：

1. **`stmt_has_label` 用错了谓词**（10 个对象的直接死因）。死分支折叠的拦网是「被丢的臂里有
   标号 ⇒ 可能有外部 `goto` 跳进来 ⇒ 不折」，它原先用只认 `__inl<N>__end`（及嵌套形）的
   `is_inline_exit_label`。而内联器给被搬进来的**每一个**标号加的都是 `__inl<N>__l` 前缀：
   `include/linux/jump_label.h` 的 `static_branch_unlikely()` 取支里那句 `arch_static_branch_jump()`
   一带 `asm goto`，落点 `l_yes` 就长成 `__inl5__ll_yes`，落在要丢的臂里 ⇒ 拦网白白触发 ⇒
   `branch = ____wrong_branch_error()` 那条恒假支永远塌不掉。改成复用已有的 `is_inliner_label`
   （它的文法本来就是 `__inl<digits>__<非空尾>`，注释里连 `__inl<N>__lt_yes` 都写到了）。
   两份文法合一处：`is_inline_exit_label` 删除，`stmt_has_label` 直接调 `is_inliner_label`。
   为什么这个豁免是安全的：这类名字的 `goto` / `asm goto` 引用者与落点由同一次展开成对生成，
   整臂被丢时两者一起消失，函数里不会留下指向它的跳转。

2. **cprop 代回器缺 `Expr::Ident` 叶臂**（`iommu_amdv1.o` 死因之一，`drivers/tty/serial/8250` 的
   `DO_ONCE()` 同族）。`subst_cprop_into_stmt_exprs` 只认得赋值左侧的替换，遇到纯拷贝
   `r1 = r2` 落在 `_ => false` ⇒ 表里有 `r2 → 0` 也代不回 ⇒ 跨「有 `goto` 指向的标号」那次
   拷贝之后的 `if (r1)` 永远是运行期读。**只代源位、不代目标位**（代目标位会把 `r1 = r2`
   变成 `r1 = 0` 之后再也没人写 `r1`，实测会把活存储折丢）。

3. **内联后的语句表达式在条件位上算不出常量**（`iommu_amdv1.o` 的另一半，三格）：
   `drivers/iommu/generic_pt/iommu_pt.h:667` 的
   `pts_feature(&pts, PT_FEAT_DMA_INCOHERENT) && !pt_test_sw_bit_acquire(&pts, …)` 内联后左
   操作数是 `({ unsigned __inl1$_n = n; …; __inl1$rv = 0; goto end; end: 0; })` —— 尾值已是
   字面量 0。三格各自钉住它：
   * `const_eval::interp_block_item` 见「声明初值算不出常量」就整块拒：`__inl1$_n = n` 里的
     `n` 是**外层**函数的变量，env 里查不到。放行条件收紧成「初值是**纯读**」
     （新增白名单谓词 `init_is_pure_read`：默认「剩下形状都有效果」，volatile 限定类型、
     调用、赋值、自增、内联汇编、语句表达式、`va_arg`、复合字面量一个都不放），并且
     **不把这个名字登记为局部** —— 后面真读它照旧退回 `eval_full` 判非常量、真写它照旧撞
     「赋值目标不是本块局部」，两条都还是整块拒，所以放不进「拿脏值当常量」。
   * `semantics::discarded_value_needs_code` 对 `StmtExpr` 一律答「需要代码」⇒ 塌缩后的条件
     又被留成一条语句，右侧那个陷阱调用就发进 `.text`。新增一格：`eval(e)` 算得出整数时
     答「不需要」（凭据是求值器自己给的 —— 它已经拒过一切外部写、调用、汇编、`static`、
     回路；`eval` 只返整数，尾值是地址那一族照样留码）。
   * `&&`/`||` 与 `?:` 的丢弃判据两侧都问。按 C11 6.5.13/6.5.14/6.5.15p2，**左侧能折成常量时
     走哪条臂是确定的**，只需问会被求值的那一侧；左侧折不出常量时保持旧口径（两侧都问），
     不放宽到「猜」。

靶件与用例（都是「与 `gcc -O2`/`-O3` 逐条对齐」的产物级双向判据，`gone`/`kept` 各一半）：

| 夹具 | 用例 | 钉住的机制 |
| --- | --- | --- |
| `tests/fixtures/c/asm_goto_label_in_dead_branch.c` | `inlined_asm_goto_labels_in_dead_branch_do_not_block_const_if_folding` | 第 1 条；`trap_jumpin`（恒假臂里有**源码标签**且别处 `goto` 跳得进去）是拦网必须继续生效的反向对照 |
| `tests/fixtures/c/shortcircuit_guard_trap.c` | `short_circuit_guards_only_kill_the_side_that_is_never_evaluated` | 第 2 条 + 第 3 条第三格；`1 && 调用`、`0 || 调用` 证明「不是右侧一律可丢」 |
| `tests/fixtures/c/inlined_se_tail_const_trap.c` | `inlined_stmt_expr_in_condition_folds_to_the_gcc_reference` | 第 3 条三格；`trap_se_live`（尾值是运行期值）与 `trap_se_outer`（SE 写块外局部，求值器必须继续拒）两条反向对照 |

装置（新增）：`scripts/probe_kernel_cc.sh` —— 拿真树头文件编我自己写的 5~10 行探针（沿用
某个对象的完整 `savedcmd`，只换源文件与输出），这一族缺陷的形状高度依赖头文件里的宏展开，
手写「像内核」的裸 C 会照不出缺陷：本轮 `.work_tmp/repro_swbit_trap.c`（34 行）与
`.work_tmp/repro_static_branch_unlikely.c`（72 行）都在旧二进制上「折得动」，害得我一度以为
缺陷在别处；真正的最小件是 `/tmp/pv/p1.c`（5 行，含真 `#include <linux/jump_label.h>`）与
`/tmp/lad/s4.c`（11 行，含内联早退链）。`scripts/trap_symbol_replay.sh` —— 陷阱符号清单上的
对象逐个重放并报引用。

误判的纠正（§5.0o 里那条 s11 叙事要打折读）：本节原先写「内核里 `__pt_no_sw_bit`/
`____wrong_branch_error` 这 7 个对象就是跨标号拷贝那个形状」，实测**只对了一小部分**：
`s11` 那形确实是第 2 条，但 17 个 `____wrong_branch_error` 死因是第 1 条（拦网），
`iommu_amdv1.o` 死因是第 2 条 + 第 3 条。另外三处本轮试过又回退的假路（别再走）：
①「单尾 return 不发出口标签」；② 把 `fold_branches_block` 尾部的 `fold_branches_stmt` 换成
`fold_branches_operand`（下探后再试一次本节点塌缩）—— 与第 3 条修完后已无观测差异，回退；
③ 拿 `readelf -r` 找 `map_one` 的真调用当作证据 —— 证据本身是对的，结论（「标号紧邻规则没跑」）
是错的。

### 5.0b 旧版：靠 `make clean` 从 0 重跑（仅用于最后那一次可复现验证）

Kbuild 的 `if_changed` 只比对**记录的命令行串**，而内核 TU 的命令行里写的是包装脚本路径
（`scripts/dcc-kernel-cc.sh`）。dcc 二进制换了一版，Kbuild 并不认账，于是旧二进制编出的
`.o` 会混进 `vmlinux.o` —— 产物既说不清是哪一版编的，也不可复现。包装脚本在 dcc 失败时
还会回退 gcc，回退产物同样不会被 make 察觉。所以每修一个缺陷都要按「二进制级判据」清一次：

```bash
# 1) 建当前源码的二进制
 cargo build --release -j2

# 2) 清掉「早于该二进制」+「gcc 回退」两类 .o（宿主工具 scripts/、tools/，以及有意
#    交给 gcc 的 16 位 setup 代码 arch/x86/boot/ 顶层、realmode/rm/ 与 rmpiggy.o
#    一律不碰；boot/startup/、boot/compressed/、realmode/init.o 是 64 位 dcc TU，照常参与判据）
#    ⚠ 只对 **dcc 树** 使用。脚本缺省就是 `~/linux-7.1-dcc`，并且会拒绝对没有
#    `dcc-kernel-cc.sh` .cmd 指纹的树动手（那通通常是 gcc 参照树）——
#    误删参照树会把后面的全部 A/B 对照基线拆掉（详 §5.0d 与报告 §3.18）。
bash scripts/kernel_purge_stale_objects.sh /path/to/linux-7.1-dcc target/release/dcc

# 3) 只清 gcc 回退产物（本轮改动不改变其它产物时用它，省一次全树重编）
bash scripts/kernel_purge_stale_objects.sh --gcc-fallback-only /path/to/linux-7.1-dcc target/release/dcc

# 4) 再跑（新目标：HOSTCC 不给了 gcc，并行度 -j2）
 make CC=/path/to/scripts/dcc-kernel-cc.sh \
       HOSTCC=/path/to/scripts/dcc-host-wrapper.sh -k -j2
```

磁盘余量也要盯：dcc 的头文件缓存落在 `~/.dcc/cache`（内容寻址、可安全删），编全树时
每轮都能长到几十 GB，按时间清旧条目即可：`find ~/.dcc/cache -type f -mmin +30 -delete`。
整轮释放用 `bash scripts/cleanup_disk.sh`（先 `--dry` 看占用分布）—— 但**注意它的第 6 项会清空
`.work_tmp/`**，那里面是全树判据脚本（`kwaudit.sh`/`rb.sh`/`kwghostscan.sh`/`xsym.awk` 等），
跑前先 `tar czf /root/worktmp-backup.tgz .work_tmp`，跑完解回来。

### 5.1 基本用法

```bash
cd /usr/src/linux

# 使用 DCC 作为 C 编译器
make CC=dcc -j$(nproc)

# 或者使用绝对路径
make CC=/usr/local/bin/dcc -j$(nproc)
```

### 5.2 带详细日志

```bash
make CC=dcc V=1 -j1 2>&1 | tee /tmp/dcc_build.log
```

### 5.3 只编译特定目标

```bash
# 只编译 init/main.o
make CC=dcc init/main.o

# 只编译内核核心
make CC=dcc kernel/
```

### 5.4 已知限制

DCC 已成功编译完整 Linux 7.1 内核并生成 vmlinux，且 QEMU 启动验证通过，但仍有以下注意事项：

| 问题 | 原因 | 状态 |
|------|------|------|
| 解析器错误 | 复杂宏展开后的 GNU 构造 | asm-offsets **解析已通过**（2026-07-16） |
| 语义/类型检查 | percpu、thread_info 等内联宏展开类型不完整 | asm-offsets **已通过**（2026-07-17） |
| codegen 不完整 | 部分 x86_64 指令尚未实现 | 持续改进中 |
| vmlinux 生成 | 完整内核链接 | ✅ **已完成**（2026-08-25，16MB ELF，零 objtool 错误）。⚠ 口径提醒：那轮不要求「编译阶段零 GCC 代工」；本轮（2026-10-28）正在用 `kwgccaudit.sh` 重验，已有的最严证据是 §「链接期预检」那节的部分合并 A/B 全绿，`vmlinux`/`bzImage` 本身还在全树后续 |
| QEMU 启动 | bzImage 串口启动日志 | ✅ **已验证**（2026-08-27，需启用 CONFIG_SERIAL_8250_CONSOLE）；harness 已于 2026-10-28 用参考内核重跑实（见上节 `kwbootltp.sh`） |
| `.note.GNU-stack` 不发 | GAS 对每个 C 文件会写 `.section .note.GNU-stack,"",@progbits`；dcc 不写。实测全树 **830/837** 个对象缺该节，而 gcc 参照树只缺 35 个（都是未标注的 `.S`） | 不挡构建：顶层 `Makefile:1192` 无条件 `KBUILD_LDFLAGS += -z noexecstack`（且 BFD ld 额外 `--no-warn-rwx-segments`），最终 vmlinux 的栈仍是非执行。代价是 `ld -r` 阶段会刷一大片 `missing .note.GNU-stack section implies executable stack` 告警（是告警、不是错误），以及与参考产物的节布局差异。修法是在 `.s` 发射尾部固定补一条，但它会改**每个**对象 ⇒ 与从 0 验证同轮 |
| **initramfs 启动到 init** | 内核执行 "Run /init as init process" | ✅ **已验证**（2026-08-27，busybox init 脚本成功执行） |
| DEFINE() .ascii 输出 | -S 模式下发射 `.ascii "->..."` 行 | ✅ **已完成**（2026-08-28，与 GCC 输出一致） |
| .S 文件预处理 | 续接行处理与 GCC 行为一致 | ✅ **已验证**（entry_64.S 预处理后可通过 GAS 汇编） |
| const_eval offsetof | 复杂内核结构体偏移量求值 | ✅ 当前 HEAD 下 `arch/x86/kernel/asm-offsets.c` 由 dcc 编出 `asm-offsets.s` 并重生 `include/generated/asm-offsets.h`（2026-10-25 实测） |
| .S 文件全链路编译 | dcc 预处理 + gcc fallback | ✅ **已完成**（2026-08-28，dcc-kernel-cc.sh 用 dcc 编译 .S，失败自动回退 gcc；51 个 .S 文件全部处理，QEMU 启动验证通过） |
| 字符字面量跨行扫描 | 注释中 `'` 吞掉后续行宏展开 | ✅ **已修复**（2026-08-28，三处扫描循环增加 `\n` 终止条件） |
| inline asm 的 GAS 宏（`.macro`/`.irp`/`.ifc`/`.set`） | 内核 `_ASM_EXTABLE_TYPE_REG` 的 type 字段靠它 | ✅ **已实现**（2026-10-25：嵌套块尾计数、形参/实参与 `\@`、`.set` 符号表、整数表达式补移位与按位运算、先解析操作数再展开宏） |
| 自定义节的 flags / 对齐（`SHF_MERGE`、`sh_addralign`） | objtool 校「节长 = 条目长整倍数」 | ✅ **已修复**（2026-10-25：并行合并不再压平 flags；非 merge 节默认对齐 1） |
| ALTERNATIVE 表条目长与替换节布局 | `.byte 773b-771b` / `.byte 775f-774f` 算错 ⇒ objtool 报 `empty alternative entry`；`.skip` 根本没实现；可执行 pushsection 的目标节按 16 垫 ⇒ `.altinstr_replacement` 节长被吹大三倍 | ✅ **已修**（2026-10-25：实现 `.skip`/`.space` 与测距遍、GAS 关系运算为真给 -1、标号按「真身所在节」结算（`pushsection_label_offsets` 覆盖可执行区间）、可执行节追加只按节自己的对齐）。三层嵌套 `ALTERNATIVE` 的 `.altinstructions` 与 `.altinstr_replacement` 已做到与 gcc 逐字节一致（用例 `nested_alternative_entries_match_gas_byte_for_byte`） |
| 函数对齐（`-falign-functions=N` / `-fmin-function-alignment=N`） | 内核 `CONFIG_FUNCTION_ALIGNMENT=16` 靠它把每条函数摆到 16 字节边界，并顺带把 `.noinstr.text`/`.init.text`/`.entry.text` 的 `sh_addralign` 抬到 16 | ✅ **已实现**（2026-10-25：两个写法都解析并送进 `Target::align_functions`，发射与自定义节取同一口径）。包装器不再吞掉 `-fmin-function-alignment=*`，而是在交给 gcc 时重写成 `-falign-functions=*`——本机 gcc 13 认不得新写法，而内核的 `cc-option` 探测问的是包装器，吞掉会让探测谎报「支持」 |
| `-fcf-protection=branch` 被接受但丢弃 | 内核在 `CONFIG_X86_KERNEL_IBT=y` 时经 `cc-option` 必传它，dcc 当时只在 `cli.rs` 里「接受并忽略」⇒ C 代码里一条 `endbr64` 都没有（gcc 参考 `arch/x86/mm/fault.o` 有 2 条）。objtool `--ibt` 对此只报 WARN 不挡构建，所以缺陷一直藏着，但在 IBT 硬件上间接调用会 `#CP` | ✅ **已实现**（2026-10-28：`-fcf-protection=branch/full` 进 `Target::cf_protection`，发射点 `emit/functions.rs` 在 patchable 前缀 nop 之后、函数符号处发 `F3 0F 1E FA`；用例 `cf_protection_emits_endbr64_after_patchable_prefix` 覆盖「开启/未开/`none`」三档，已做撤掉修复即变红的反向验证） |
| `endbr64` 发射面比 gcc 宽 | GCC 只在「可能被间接转移命中」的入口放 `endbr64`（实践上即外部可见的函数），dcc 目前每个函数入口都放。实测四个 TU 的两边条数：xstate 93/24、events core 167/69、bugs 143/53、mm_init 112/29 | ⚠️ **已知发散**（2026-10-28：方向在安全侧，多一个 `endbr64` 不会让 IBT 故障；代价是 `.text` 膨胀、以及与参考产物的字节差异。真对齐需要「哪些函数允许被间接转移」这个事实进入 codegen，与链接期的可见性判定一起评估） |
| 强制内联的调用点形状覆盖 | `__always_inline` 体里带 `"i"(形参)` 时，调用点没展开就不是「少一次优化」而是**编不出来**：编译器必须另发一份 callee 副本，而那份副本里的立即数操作数拿到的是运行时的形参值（`error[E0311]`）。循环条件、语句表达式里层、实参里的调用三形都踩过这一坑 | ✅ **已修**（2026-10-28：`inline_expr` 新增 `Expr::StmtExpr` 一臂、实参下探后重试一层并在该重试里放宽「实参有副作用」否决、`While`/`Do` 两支补 cond 下探（与 `For` 同口径，由 `DCC_INLINE_LOOP_COND` 管，默认开）。用例 `forced_inline_in_loop_conditions_expands`、`forced_inline_inside_stmt_expr_expands`） |
| 循环条件里的强制内联与 `__builtin_constant_p` 死分支互撞 | 把 `while`/`do-while` 的条件送进内联器后，`kernel/sched/wait_bit.o` 从能编变成报 `E0311: ... (expression: (1 << (__inl$_nr & 7)), in function __wait_on_bit)`：`do { … } while (test_bit_acquire(…) && !ret)` 展开后，callee 体里 `__builtin_constant_p(形参)` 的三元**两条分支都被展开**，死分支带进 `Stmt::Label` 之后 `fold_branches_block` 以「goto 可跳入死分支标签」为由拒剪 ⇒ 那条不会执行的分支里的 `"i"(形参)` 被当成活代码编了 | ✅ **已修**（2026-10-28：三道一起 —— `fold_expr` 的 `Expr::Cond` 臂改走 `const_eval` 整体求值、`subst_cand_reads_items` 候选表接「链」、新增 `prune_spliced_items` 在递归展开前先代回+折分支。`DCC_INLINE_LOOP_COND` 翻回默认开，`wait_bit.o`/`ptrace.o`/`build_utility.o` 开着时实测全部能编；闸门留着仅供配对 A/B 逐形退回） |
| `.section X,"a"` + `.previous` 退不回原节 | 内核 `EXPORT_SYMBOL`（`include/linux/export.h` 的 `___EXPORT_SYMBOL`）就是这个形状。以前 `.section` 把**即将进入的节**同时压进 `section_stack` 与 `pushsection_stack`，`.previous` 先弹出它、又同一个下标原样压回去 ⇒ `pushsection_stack` 永远留着那个早已退出的数据节，之后同一函数里每条 asm 都按「在非执行 pushsection 内」处理，数字标号按那个数据节的坐标登记。实测 `mm/mm_init.o`：`ANNOTATE_REACHABLE(1b)` 的 5 个标号全被记成 `.export_symbol` + 0x80，链接期 objtool 报 `bad .discard.annotate_insn entry: 0 of type 8` ⇒ `vmlinux.o` 做不出来 | ✅ **已修**（2026-10-28：`.section` 存「切换前那个节」（栈空用 `usize::MAX` 哨兵），`.previous` 按它恢复；用例 `section_previous_restores_prior_section`，最小复现 `a7.c`：标号从 `.export_symbol`+0x08 回到 `.text`+0x4b） |
| 内联 asm 体内的 `call` 不护延伸池家 | 内核跳进另一个 C 函数的 asm（`call write_ibpb`、`call rep_stos_alternative`）只写 `ASM_CALL_CONSTRAINT`，那份 clobber 名单里没有寄存器号；而按 ABI 这次调用毁掉的是全部 caller-saved GP。住在 r8/r9 里的家会被静默抹掉，而 `call_label` 的守卫只会把编译顶死（`error[E1136]`） | ✅ **已修**（2026-10-28：体内检出 `call` 就把 SysV caller-saved GP 并进既有守护名单（push/pop 区间，操作数号依旧排除在外交给写回路），家全有着落时开一层 `asm_call_window` 不再误报；`asm goto` 那条路径不插守护，传 `false`。用例 `asm_body_call_guards_ext_homes`） |
| `.long (1b) - .`：宏留下的括号让数字标签解不开，发出名叫 `(1b)` 的 UND GLOBAL 符号 | 编译全绿，链接期 objtool 报 `special: can't find orig instruction`（`ldt.o`、`fpu/core.o`、bugs.o、events core.o 都有）；真正的问题在运行期：`__ex_table`（内核异常表）每一条都指向不存在的位置 | `_ASM_EXTABLE(from,to)` 展开就是 `.long (1b) - ., (2b) - .`；`parse_asm_data_expr`/`resolve_asm_data_symbol` 拿整串去分类，`(1b)` 认不出是数字标签就原样当符号名用了 | ✅ **已修**（2026-10-28：`strip_outer_parens` 先把成对的外层括号剥掉再分类（`(a)+(b)` 不配对所以不动），两处入口共用；GAS 本来就把括号当分组。修后 `ldt.o` 里 `(1b)`/`(2b)` 两个 UND 符号消失，`objtool --link --ibt/--noinstr/--unret --noabs` 全部 rc=0。用例 `extable_parenthesized_numeric_labels_resolve`） |
| objtool 报 `.altinstr_replacement+0xNN: can't find jump dest instruction at …+0xMM` | 同上形状（只在 `ld -r` + objtool 阶段暴露），站点 bugs.o(+0x9) / events core.o(+0x1d) / xstate.o(+0x46)；`ldt.o`/`mm_init.o` 在本轮修完异常表后已四项判据全 ok | 未定案，但已收敛到具体位置。两边的差：gcc 在那四条 `jmp` 上发的是 `R_X86_64_PC32 .text + 0xed`（位移域留 0），dcc 把位移直接写死（`e9 20 00 00 00`）且 **没有重定位**。而这条规则本来已有实现：`codegen.rs::patch_local_jumps` 拿到 `move_ranges`（自定义函数 + `pushsection_code_ranges`）后用 `final_text_loc` 比较两边的「最终节」，不等就发重定位 —— 即 `src_sec == dst_sec` 被误判为相等。**定案**：误判的原因是 `move_ranges` 被整行拿错一格 —— `final_text_loc` 按 `(发射起点, 发射长度, 目标节下标, 节内偏移)` 读，`compile_rel` 递的却是 `(func_start, emitted_start, emitted_size, sec_idx)`，于是节号位置上是**发射长度**（探针里看到的 `sec 359`/`sec 353` 就是 `emitted_size + 7`），两边总是算成同一个不存在的节号 ⇒ 跨节跳转被当同节处理。运行期后果比链接期报错更严：`apply_alternatives` 换上去的替换代码会跳到指令中段 | ✅ **已修**（2026-10-28：`codegen.rs` 与 `elf/resolve.rs` 两处统一成同一个含义 —— 之前一份用 `(start,end,…)`、一份用 `(start,size,…)`，同一个四元组两套含义就是错位的温床；含义写到了声明旁边。实测：traps.o / bugs.o / events core.o / xstate.o 单跑 objtool 的 `--ibt/--noinstr/--unret/--orc` 已**全 ok**，`ld -r` 合并后同样全 ok。用例 `cross_section_jump_out_of_exec_pushsection_carries_reloc`；它只钉「跳重定位」这条不变式，拿旧二进制跑不会变红，真凭据是上述四个内核 TU 与合并对象的 objtool 结果）。本轮曾试图在 `elf/resolve.rs::resolve` 里再加一层同样的判断，实测发现那里根本走不到（真正回填的是 `patch_local_jumps`），已回退 —— 同一规则不留两份 |
| objtool ORC 告警 `return with modified stack frame` | dcc 用 `r13` 当帧基址（序：`push %r13; mov %rsp,%r13; sub $N,%rsp`，尾：`mov %r13,%rsp; pop %r13; jmp __x86_return_thunk`），而 objtool 的 ORC 生成只认 `rbp` 作帧指针，于是把这些函数判成「返回时栈帧被改」。同一 TU 两边实数：`gdt_idt.o`+`la57toggle.o` 合并后 dcc 6 条告警 / gcc 0 条 | ⚠️ **已知**（2026-10-28：`objtool --link --ibt --noabs` 仍 rc=0，`--orc` 只 WARN 不挡构建；影响的是 oops 回溯可靠性，不影响编译与启动。真修需要把帧基址换回 `rbp`，属后端全局改动，单独评估） |
| realmode 路径一刀切委托 gcc | 包装器曾按 `*/realmode/*` 整目录委托，把 64 位的 `arch/x86/realmode/init.c`（`.init.o.cmd` 里没有 `-m16`）也白送给了 gcc | ✅ **已修**（2026-10-28：删掉路径规则，只留 `-m16/-m32` 判据——26 个真 16 位 C 仍由 gcc 代编，`realmode/init.c` 回归 dcc；`kernel_purge_stale_objects.sh` 的排除同步收窄为 `realmode/rm/`、`rmpiggy.o` 与 `boot/` 顶层） |
| 16 位 setup 代码（`arch/x86/boot/*.c`、`arch/x86/realmode/rm/*.c`） | dcc 只有 x86_64 后端，没有 `code16gcc` + `-mregparm=3` 的 16 位路径；这 26 个 C 文件是设计性例外，包装器按 `-m16/-m32` 委托 gcc 并留 `GCC-DELEGATE` 痕迹（不是缺陷，不做追踪） | ⚠️ **长期例外**（2026-10-28 复核：其余 64 位 TU 均已由 dcc 编） |
| `~/.dcc/cache` 无限增长 | 全树单轮可涨到 38 GB / 44 万条目，且无目录环境变量与淘汰策略 | ⚠️ **未修**（本轮靠外部 `find ~/.dcc/cache -type f -mmin +30 -delete` 控住，已写进 §5.0；缓存是内容寻址，删任何条目只会多算不会错值，适合加容量上限淘汰） |

## 6. 调试技巧

### 6.1 查看预处理输出

```bash
dcc -E input.c > /tmp/preprocessed.c
head -100 /tmp/preprocessed.c
```

### 6.2 查看 AST

```bash
dcc -dump-ast input.c
```

### 6.3 详细模式

```bash
dcc -v input.c -o output.o
```

### 6.4 使用 GDB 调试 DCC

```bash
# 在本地 Linux 上编译 debug 版本
cargo build  # debug 模式

# 启动 GDB
gdb ./target/debug/dcc

# 设置断点
(gdb) break parser.rs:100
(gdb) run input.c -fsyntax-only

# 单步调试
(gdb) next
(gdb) print self.cur
```

### 6.5 对比 GCC 预处理输出

```bash
# DCC 预处理
dcc -E input.c > /tmp/dcc_out.c 2>/dev/null

# GCC 预处理
gcc -E input.c > /tmp/gcc_out.c 2>/dev/null

# 对比
diff /tmp/dcc_out.c /tmp/gcc_out.c | head -50
```

## 7. 常见问题

### Q: 报错 "unknown C compiler"

A: 内核构建系统会检测 CC 编译器。确保 DCC 能正确响应版本查询：
```bash
dcc --version
# 应输出: dcc 0.1.0
```

### Q: 预处理输出过大

A: 检查是否有头文件被重复包含。DCC 的 include guard 机制通过文件路径 canonicalize 实现，确保所有路径可正确解析。

### Q: `#error` 误触发

A: 通常是因为宏定义体中包含注释（如 `HZ CONFIG_HZ /* comment */`），导致 `#if` 表达式求值失败。DCC 已内置 `strip_comments()` 处理此情况。如果仍遇到，请报告。

### Q: 解析器报 "expected type" 错误

A: 可能是遇到了 DCC 尚未支持的内核构造。使用 `-dump-ast` 查看已解析的部分，
定位错误位置。`asm-offsets.c` 已于 2026-07-17 全链路通过。

## 8. 性能对比

在典型开发机器上的参考数据：

| 文件 | GCC 预处理行数 | DCC 预处理行数 | DCC 解析时间 |
|------|-------------|-------------|------------|
| init/main.c | ~95K | ~161K (1.7x) | < 0.5s |
| kernel/panic.c | ~57K | ~108K (1.9x) | < 0.3s |
| kernel/sys_ni.c | ~5K | ~18K (3.6x) | < 0.1s |

## 9. 参考资料

- DCC 源码: `src/preproc.rs` (预处理器), `src/parser.rs` (解析器)
- Linux 内核构建系统: `Documentation/kbuild/` 
- GCC 兼容性: `docs/roadmap.md`
