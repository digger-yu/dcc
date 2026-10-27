# DCC 脚本索引

`scripts/` 下 56 个脚本（另有子目录 `git-hooks/` 里的钩子本体，不算独立脚本）的分工与
调用方。判据只有一个——谁在调用它：Makefile 目标、
`.github/workflows/*.yml`、`tests/`、`docs/`，或另一个脚本。

## 共通约定

| 变量 | 含义 |
|------|------|
| `DCC` | 待测编译器路径。默认几乎都是 `target/release/dcc`；唯一例外是 `test_kernel_features.sh`，它默认 `./target/debug/dcc` |
| `DCC_REPO` / `REPO_ROOT` | 仓库根，多数脚本自行推导，可覆盖 |
| `KERNEL_DIR` | 内核树路径（内核那批脚本用；Makefile 默认 `/root/linux-7.1`） |
| `JOBS` | 并发度。Makefile 默认 `nproc`，但跑测试用例脚本时按 `-j2` / `--test-threads=2` 限流 |
| 退出码 | 0 = 通过；非 0 = 已点名问题。缺外部工具时打印显式 `SKIP` 而不是静默通过（如 `check_asm_text_parity.sh` 的 `ASM-PARITY-SKIP`），需要"不许 SKIP"时用各自的 STRICT 开关 |

## Tier 0（`make test`，PR 必绿）

| 脚本 | 用途 |
|------|------|
| `cargo_test_summary.sh` | 跑 `cargo test --release` 并给出跨 target 总计 + 失败/忽略清单；退出码透传 cargo，自身不判绿。原始日志最上面带元信息头（开始/结束时间按北京时间、命令行、git 版本、耗时、cargo 退出码）—— cargo 原话开头必然是编译 `warning:`，没这个头看着像日志被截断 |
| `check_fixtures.sh` | fixture 引用完整性 / 命名 / 目录规则，硬失败无过渡开关 |
| `check_fixtures_selftest.sh` | 元测试用例：5 类反向注入逐条 `exit 1`，证明上一条真的会红 |
| `test_all.sh` | `tests/fixtures/smoke` 端到端冒烟的薄编排，不复制断言逻辑；由 `make test-all` 接入 |

## 本地 git 钩子（调用方是 git 自己）

| 脚本 | 用途 |
|------|------|
| `git-hooks/pre-commit` | 提交前闸门：`torture_test.sh all` 的 fail 必须为 0，否则拒绝提交。具体判据（五条 fail-closed 条件、限流默认值、日志位置）以脚本头注释为唯一真源，本表不重述 |
| `install_git_hooks.sh` | 把上面那个钩子复制到 `.git/hooks/`（`make hooks`）；只复制不改任何 git 配置，内容不同时默认不覆盖，`--force` 才先备份再覆盖 |

## 产物级不变量（`make opt-eq` / `rex-hygiene` / `golden-cross` / `pipe-structure` / `disasm-cov`，及 CI）

| 脚本 | 用途 |
|------|------|
| `check_opt_equivalence.sh` | 优化档位执行等价：全语料 × `-O0/-O2/-O3` 与 gcc 差分 |
| `check_rex_hygiene.sh` | 以 objdump 为权威扫产物：多余 REX 位 = 0、`%ah..%bh` = 0 |
| `arm_mips_golden_baseline.sh` | ARM/MIPS 字节基线；`make golden-cross` 跑 `verify` 模式——基线缺失或产物漂移都置红，禁止 CI 里悄悄重写 |
| `check_pipe_structure.sh` | P16 软件流水的两个独立读数交叉：`DCC_PIPE_DUMP` 的接受/拒因，与 objdump 比较默认产物和 `DCC_SSA_OFF=pipe` 产物 |
| `check_disasm_coverage.sh` | 自家 `-S` 反汇编逐字命中率；`make disasm-cov` 传 `LEVELS="0 2" CEILING=0`，CEILING 必须是显式数字，否则上限=本次实测值、永不红 |
| `check_reproducible_output.sh` | 编译产物可复现性（CI `ci.yml` 的 reproducibility job） |
| `check_asm_text_parity.sh` | `-c` 直产 vs `-S --asm-fallback=gas` 经真 `<triple>-as` 汇编后的指令文本一致（CI `ci.yml` 的 parity job） |
| `probe_backend_features.sh` | 后端能力行为探针，与 `docs/backend_feature_matrix.md` 逐格对账，不一致即 CI 红（`ci.yml` 的 matrix job） |

## GCC torture（`make torture` / CI `torture.yml`）

| 脚本 | 用途 |
|------|------|
| `setup_torture.sh` | 稀疏下载 gcc.c-torture 到 `tests/gcc-torture/`，并按源码 `#include` 补齐 `gcc.dg` / `gcc.target` 兄弟文件 |
| `torture_test.sh` | 总驱动（`compile` / `execute` / `all`） |
| `torture_compile_test.sh` | compile 档 |
| `torture_execute_test.sh` | execute 档 |
| `torture_summary.sh` | 解析两份日志生成 GitHub Actions Summary |
| `torture_gate.sh` | 与 `tests/skip/torture_known_fail_*.txt` 基线对比，只允许失败集合收缩 |

## GCC dg（CI `gcc-dg.yml`）

| 脚本 | 用途 |
|------|------|
| `gcc_dg_test.sh` | `tests/gcc-dg` 语料驱动，跳过清单在 `tests/skip/gcc_dg_skip.txt` |
| `gcc_dg_summary.sh` | 解析日志输出 Actions Summary |

## 上游官方测试套（手工，gcc 基线对拍）

跑的是上游自带的测试套（不是 `tests/fixtures/` 语料），调用方是人或 `.github/workflows`
里的手动作业（`sqlite-suite.yml`，只有 `workflow_dispatch`）；两者共享同一把
机器级独占锁 `/tmp/dcc-suite-gate.lock`（套件里的时序类用例对负载敏感，并发互跑会把
基线污染成假红）。结果数据落 `docs/dcc-lua-sqlite-build-verify-report.md`。

| 脚本 | 用途 |
|------|------|
| `setup_sqlite_suite.sh` | 拉取/解压 SQLite **完整源码树**（含 `test/*.test` 与 `configure`，不是 amalgamation）到 `$SQLITE_SRC`；本地与 CI 同一入口 |
| `check_sqlite_suite.sh` | SQLite 官方 TCL 套（默认 `test/quick.test`）gcc 基线 vs dcc 各档。判据取**失败测试名集合**的差（`comm -13`），gcc 自己也失败的项不判 dcc 红；判红两类：`DCC-BUILD-FAIL` / `SUITE-DIFF`，另有一类只点名不直接判红：`SUITE-INCOMPLETE`（没跑到汇总行）。退出码：0 PASS / 1 判红 / 2 前置不满足（含基线自己跑不完——参照集残缺时不再比 dcc）/ 3 抢不到锁 / 4 只有 `SUITE-INCOMPLETE` 且无新增失败名（历史上这一档容的是「dcc 跑 quick.test 中途 SIGSEGV」那族缺陷，最后一个已在 2026-10-27 修完，四档都能跑到汇总行；再出 rc=4 就是新问题，得看日志。把这类也算 1 的话，超时/机器抖动也会判红，红/黄由调用方定）。`SUITE`/`LEVELS` 的分隔符空格、逗号、分号均可（`LEVELS="0,1,2,3"` 与 `"0 1 2 3"` 等价，非 0-9 的档位当场报错）—— workflow_dispatch 输入框里人手很容易写逗号版，旧写法会把整串当成一个档拼成 `-O0,1,2,3` 交给编译器。以 root 跑会自动降权到 `$SUITE_USER`（`attach-6.2` 靠权限断言，root 下整套提前退出，故 root 而缺 `runuser` 时直接报错而不是默默当 root 跑）；非 root（CI 的 `runner`）直接用当前身份跑，因此提前叫一次 `$DCC --version` 把权限/缺库问题与编译器本身坏了分开。testfixture 用 capture 桩拿 `main.mk` 的真实 argv 后**逐 TU 独立进程编译**（单进程扫 88 个 TU 不回收内存，会在 `sqlite3.c` 前被 OOM 杀掉）。`SUITE-INCOMPLETE` 的档也会在崩溃前的输出里算一次新增失败名：否则「没跑到汇总行」会把「跑到的部分里有新失败」一起吞掉，`INCOMPLETE` 就成了另一种假绿 |
| `check_lua_suite_diff.sh` | Lua 5.5 官方 `all.lua` 三条路径（路径 A = `ltests` 内部断言构建，路径 B-full = stock `make linux` 产物跑全量，路径 B = 同一 stock 产物加 `-e "_U=true"` 用户模式）× dcc `-O0`…`-O3` vs gcc `-O$REF_LEVEL` 基线。除结构判据（rc、`final OK !!!`、`***** FILE` 个数硬相等、进度点宽带）外，还跑两条自证：**同一棵 gcc 树跑两遍归一化后必须相同**（证明噪声分类表完整），以及往基线里注入语义差异**必须逐个判红**（证明本脚本不恒绿） |
| `check_sqlite_verify.sh` | SQLite 的「逐字同果」验证，给的是 `check_sqlite_suite.sh` 给不出的那两类证据（后者的判据是**失败名集合**，不看单文件端到端的结果行内容，也不看整负载哈希）。A 段：`select1~5.test` 逐文件跑完，与 gcc 参照比**用例结果行逐字相同** + `0 errors out of N tests` + rc=0；B 段：上游 `speedtest1 --verify` 的 **Verification Hash** 逐档 vs gcc 参照同号，并出 best-of-N 墙钟。前置依赖 `check_sqlite_suite.sh` 留下的源码树/argv/对象集（缺档就地 `BUILD_ONLY` 补齐，绝不跳档）；沿用同一把机器级锁。注意：`sqllogictest` 不在 SQLite 发布包里（独立项目），`select*.test` 是 TCL 格式只有 testfixture 认；3.54.0 树里没有 `test/evidence/`；`main.mk` 的 `src-verify`/`verify-source` 是源码完整性检查，与查询结果无关 |
| `lua_suite_norm.sh` | 上一条用的**噪声归一化对拍器**（纯 bash + 系统 ERE，不用 python；本机 awk 是 mawk，没有 gawk 的捕获数组）。20 类噪声（种子、pid、临时路径、耗时、GC 计数/内存、进度点、`done (with N kills)`…）按类别改写为占位符，每类带**取值校验**（短路边号 ∈{0,1}、随机区间 0≤lo≤hi≤1、日期成形、ppm 与 SIGINT 次数的下限），越界就退出码 2 而不是静默吞掉。`--selftest` 共 45 条：一半是“真差异必须判红”；`--audit` 逐类给出命中数与样例。退出码 0=归一化后相同 / 1=有差异 / 2=用法或越界 |

## 内核 / Linux 7.1（详见 `docs/kernel_build_guide.md`）

| 脚本 | 用途 |
|------|------|
| `kernel_full_build.sh` | 全树 `make CC=dcc -k`，收集全部失败而非首错即停 |
| `kernel_full_check.sh` | modpost / prepare0 / bzImage / QEMU / vmlinux 编排；`STRICT_FULL_BUILD=1` 时核心步骤不许 SKIP |
| `kernel_build_smoke.sh` | vmlinux / bzImage 冒烟（`BUILD_BZIMAGE=1` 才建 bzImage） |
| `prepare0_check.sh` / `prepare0_full.sh` | prepare0 窄目标 / 全链路 |
| `modpost_link.sh` | modpost 三 TU 链接 + Rust fixdep |
| `build_fixdep_dcc.sh` | 把 Rust `fixdep` 装进内核树替代 C fixdep |
| `gen_elfconfig_h.sh` | 从 `empty.o` 的 ELF 头生成 `elfconfig.h` |
| `build_initramfs.sh` | busybox(static) + `/init` 的最小 initramfs |
| `qemu_boot_matrix.sh` | QEMU 启动矩阵；`REQUIRE_QEMU_BOOT=1` 时启动失败即红 |
| `objtool_regress.sh` | `.eh_frame` FDE / ORC 回归 |
| `pahole_regress.sh` | BTF 回归（pahole 或 llvm-objcopy） |
| `vmlinux_regress.sh` | vmlinux 级 pahole / objtool 严格回归（需 ET_EXEC vmlinux） |
| `test_kernel_features.sh` | 6 项内核构造夹具：dcc `-c` → gcc 链接 → 运行，外加 readelf 的 WEAK 断言 |
| `arch_x86_compile.sh` | 批量编译 `arch/x86/` 的 C 文件，复用内核命令模板 |
| `dcc-kernel-cc.sh` | 内核编译包装：基础设施文件用 gcc，其余用 dcc |
| `dcc-host-wrapper.sh` | Kbuild `HOSTCC` 包装：dcc 编译、gcc 链接，含参数白名单翻译 |

## 自举 / 静态构建 / GCC 源码树

| 脚本 | 用途 |
|------|------|
| `bootstrap.sh` | `make bootstrap`：dcc0 → dcc1 → dcc2 与字节/语义对比 |
| `build_static.sh` | musl 全静态构建 dcc 自身（见 `docs/static_build.md`） |
| `cc1plus_bootstrap.sh` | 以 dcc 为 `CC`/`CXX` 配置并构建 cc1/cc1plus，产出进度报告，xfail 跟踪收敛 |

## C++ ABI 与差分

| 脚本 | 用途 |
|------|------|
| `cpp_abi_gcc_cross_check.sh` | 逐夹具分别用 g++（真值）与 dcc 编译链接并比对；被 `tests/cpp_abi_gcc_cross_check.rs` 调用 |
| `dcc_gcc_diff.sh` | 同一份 C 源 gcc / dcc 差分执行比对；被 `tests/integration/c_abi_vararg.rs` 与若干夹具 README 引用 |

## 性能与基线

| 脚本 | 用途 |
|------|------|
| `bench_phase_e.sh` | 阶段 E 性能冒烟：mmap + 分块 codegen + `-O2`（`docs/support_features.md` 登记） |
| `optimize_baseline.sh` | 各优化档位产物基线采集（手工） |
| `performance_benchmark.sh` | 快速编译耗时基准（手工，完整基准见 `bench_phase_e.sh`） |
| `bench_lua_sqlite.sh` | Lua 5.5.1 / SQLite **运行时**性能对比基准（手工）：四档 dcc vs gcc，含功能正确性判据（`docs/dcc-lua-sqlite-build-verify-report.md` 的数据来源）。SQLite 负载已改为**上游 speedtest1**（`--verify --testset main,cte,orm,fp`）——除计时外还比 `Verification Hash`（全部查询结果的内容哈希），跨档也必须同号；以前的自写 20 万行 SQL 只能计时、不能当正确性判据。默认 testset 不含 `rtree`：`--verify` 下第 111 步连 gcc 参照也跑不完（实测两个二进制同过 150 s 未返回）；`json`/`star` 只出空哈希，同样不进默认集 |
| `ab_pair.sh` | 同轮交替配对计时（median(r) + 95%CI + 符号检验 + 同二进制 dup 臂）：所有墙钟判据的唯一口径 |
| `gate0_probe.sh` | 产物性能根治方案的 **Gate 0 三探针**：0a 影子槽表（`DCC_SLOT_SHADOW=1`）/ 0b raw 归因两列 / 0c 按方案 §0 口径的 dcc-vs-gcc 分桶 |
| `check_facts_vs_objdump.sh` | S1 指令事实层的**外部 oracle**：拿 objdump 对同一串 raw 字节反解目的号，与 `raw_writes` 的答对账（`DCC_FACTS_DUMP=1`）；只对账强主张那一侧。`sext` 形除了查目的号还查**助记符必须属于 `movsxd`/`movsx*`/`mov r64,imm32` 那三族**（只查号抽不出「号对但扩展错」这一类错） |
| `region_home_sweep.sh` | 区内家（S4）开着时的**值等价全语料扇扫**：每份源在 -O1/-O2/-O3 各编两遍（门关 vs `DCC_REGION_HOME=1`），比的是方案 §5 的**四元组**：退出码、stdout sha256、stderr sha256、**执行目录落盘文件树** sha256（指针打印归一化手法与 `check_opt_equivalence.sh` 一致），并先跑一次「门关 vs 门关」自比对证明判据本身稳定（不稳报 `REGION-UNSTABLE` 而不是拿它判机制）。为什么不复用 `check_opt_equivalence.sh`：它比的是「同一产物四档互等」，而本机制的判据是「**开与关**两档互等」——推迟/缓存类前提被破坏时症状是别处读垃圾，只有拿门关当参照才看得见。`DEFER=0` 可只扇读侧服务那一半 |
| `hot_cold_census.sh` | computed-goto 函数的「三账 + 落脚」普查（entry/hot/out 拆开且同列），冷尾类改动的机制判据源 |
| `e_per_step.sh` | 按 opcode 执行权重折算的「每 VM 步机器指令数」E/步（静态条数不当墙钟代理时的中间量） |
| `meta_gate.sh` | 元判据：静态代理（体量 / 条每步 / taken 分支每步）能不能预测墙钟（滴定砝码 + Spearman 秩相关） |

## 生成类

| 脚本 | 用途 |
|------|------|
| `gen_coverage_index.sh` | 重写 `docs/test_coverage.md` 生成区并跑全部交叉校验；改完 `tests/**` 后运行 |
| `update_error_snapshots.sh` | 生成/刷新 `tests/fixtures/errors/*.stderr.snap`（`DCC_UPDATE_SNAPSHOTS=1`） |

## 维护辅助

| 脚本 | 用途 |
|------|------|
| `cleanup_disk.sh` | `make clean-cache`：清调试与内核编译残留、缓存，并盘点 `.work_tmp/` |
| `ci-trigger.sh` | 显示当前 remote/分支/HEAD 并 `git push` 触发 CI（见 `docs/ci_guide.md`） |

## 手工工具（无自动调用方）

不被 Makefile / CI / 测试用例调用，仅供人工复现与对照；改动相关子系统时按需运行。

| 脚本 | 用途 |
|------|------|
| `asm_compare.sh` | 同一夹具下 dcc 与 gcc 的汇编逐条对照 |
| `validate_int128_float_args.sh` | 生成 `tests/diff_test/fixtures/int128_float_args.c`（`__int128` 与 float 混排形参），分别用 gcc 与 dcc 编译运行并比对退出码 |
| `scan_all_fixtures.sh` | 全量扫描 `tests/fixtures/c/*.c`，与 gcc 逐字符差分 |
| `gcc_tree_cpp_probe.sh` | 批量用 dcc 编译 GCC 源码树 C++ TU，暴露前端缺口 |
| `repro_cpp_run.sh` | 手工复现某个 C++ 夹具：dcc 编译 → g++ 链接 → 运行 |
| `dcc_orphan_check.sh` | 检测从未参与编译的孤立源文件 |
