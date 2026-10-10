.PHONY: all build test test-all opt-eq golden-cross bootstrap kernel-full-build kernel-full-check clean-cache torture-setup torture

DCC_REPO ?= $(CURDIR)
KERNEL_DIR ?= /root/linux-7.1
DCC ?= $(DCC_REPO)/target/release/dcc
JOBS ?= $(shell nproc 2>/dev/null || echo 4)
LOG ?= $(DCC_REPO)/build.log

all: build test-all kernel-full-build

build:
	cargo build --release

# Tier 0 权威清单见 docs/tests_quality_system_plan.md §9，四条缺一即 Tier 0 不成立：
#   1) cargo build --tests   —— 全部 test target 必须能构建
#   2) cargo test --release  —— 全部 target 通过；由 cargo_test_summary.sh 跑，
#      它额外给出**跨 target 总计**（测试项总数 / 失败 / 忽略）与失败、忽略列表 ——
#      cargo 自己只按 target 各打一行，原先要自己数。退出码仍透传 cargo 的，
#      汇总器不判绿（否则就是新的假绿源）。它自带 -j2 / --test-threads=2 限流，
#      所以这里**不传 JOBS**（本文件默认的 JOBS=$(nproc) 会把机器顶满）；
#      要改并发用 `JOBS=... TEST_THREADS=... make test` 显式覆盖。
#   3) check_fixtures.sh     —— fixture 门禁本体
#   4) check_fixtures_selftest.sh —— 元门禁（证明 3 真的会红）
# 失败即红 —— 不设 `|| true`，不做任何降级。
test:
	cargo build --tests -j2
	DCC_REPO="$(DCC_REPO)" bash scripts/cargo_test_summary.sh
	DCC_REPO="$(DCC_REPO)" bash scripts/check_fixtures.sh
	DCC_REPO="$(DCC_REPO)" bash scripts/check_fixtures_selftest.sh

# test-all = Tier 0 全量 + tests/fixtures/smoke 端到端冒烟（scripts/test_all.sh 只做薄编排，
# 不复制任何断言逻辑；以前它没有任何调用点，此处接入）。门禁不在这里重复跑。
# 末尾再接入优化档位执行等价门禁：Tier 0 不跑 -O 档位，而优化器的正确性定义恰好
# 是「不改行为」—— 本轮修完的一批静默误编译（死循环/SEGV/截断）全部由它暴露。
opt-eq: build
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" JOBS="$(JOBS)" bash scripts/check_opt_equivalence.sh

# x86_64 产物的 REX 卫生不变式（以 objdump 为权威）：多余位 = 0，`%ah..%bh` = 0。
# 为什么不只靠 `rex_matrix` 那套穷举矩阵：矩阵只能覆盖能被独立调用的发射点，而表达式
# 发射器（位域/原子/蹦床…）藏在 IR 上下文里。这条门禁扫**产物**，所以任何发射点写坏
# 都会被点名（§7.26：本轮就是靠它把 42 条存量一次性点名到 0）。
rex-hygiene: build
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" bash scripts/check_rex_hygiene.sh

# 交叉后端的字节基线（arm/mips）。`verify` 模式：基线缺失或产物漂移都置红，
# 不允许在 CI 里悄悄重写基线。这个门禁曾经没有任何调用点且实为空跑（夹具路径全部
# 不存在、目标开关写错），现在既修了脚本也接进了这里。
golden-cross: build
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" bash scripts/arm_mips_golden_baseline.sh verify

# P16 软件流水的产物级核对。流水有一类失败是 gcc 差分**查不出来**的：准入闸门被重构
# 弄得永不触发时，产物只是「少优化」，依旧与 gcc 逐字节一致。所以这里用两个独立读数
# 交叉确认：`DCC_PIPE_DUMP` 的接受/拒因记录，与 objdump 比较「默认」和
# `DCC_SSA_OFF=pipe` 同一函数的反汇编。反向对照已验：给 dcc 包一层永久
# `DCC_SSA_OFF=pipe` 后本门禁报 6 项并 exit 1。
pipe-structure: build
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" bash scripts/check_pipe_structure.sh

# 自家反汇编（`-S` 读数）的逐字命中率。`# UNKNOWN` 不影响产物，所以没有任何用例会
# 因它变多而红；而 rex_matrix 那 15 个矩阵的判据止于「读回来的名字必须是意图那个」，
# 读不出就没读数。CEILING 必须给**显式数字**：不设时上限等于本次实测值，这条判据
# 就永远不会红。当前值 = 实测 0（指令行分母 278270：-O0 137782 + -O2 140488，全语料
# 读不出 0 条 ⇒ 此后**任何一条** UNKNOWN 都会红）。历史：235 →（补齐反解码器）26 →
# （字符串指令族 STOS/SCAS/CMPS + rdtsc/int3/cwt + 三处 REX 路径重复 take 造成的整页
# 错位）13 → 0。降到 0 靠的是四处根因，没有一处是「把名字猜出来」：
#   * ALU `/r` 收敛成一张按位算的表（旧的四份清单同时漏了 adc/sbb）；
#   * MOVSXD 曾把 REX.B 双重施加到 rm ⇒ `rm==4`(SIB) 变成 12 ⇒ 漏读 SIB 与位移 ⇒
#     长度少算 ⇒ 后面整条指令流错位；
#   * x87 D8..DF 按 objdump 逐格对账补齐真名（mod=3 的 512 格 + 内存形 64 格），旧实现
#     打的是 `f???` 占位符；
#   * shift/rotate `/digit` 收敛成一张表（REX 那三份漏了 rcl/rcr，兜底臂打成假助记词
#     `shift`）。
# 本仓的纪律：CEILING 只按当场实测写死，脚本自己会提醒「少了 N 条就砍」。
# 残留清单由脚本自己按字节序列聚合打印，就是待补清单。
# 这条指标有两个盲区，所以脚本里另有一条每函数指令条数探测器（详见脚本注释）：
# 错位之后读出来的是**有名字**的假指令（实测残渣 `fmuls 99(%rcx)`、`movsxd (%r12)`），
# `# UNKNOWN` 一条都不报；`f???`、`shift` 这类有名无实的占位符同样是计数盲区。
disasm-cov: build
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" LEVELS="0 2" CEILING=0 bash scripts/check_disasm_coverage.sh

test-all: test
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" bash scripts/test_all.sh
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" JOBS="$(JOBS)" bash scripts/check_opt_equivalence.sh
	$(MAKE) rex-hygiene
	$(MAKE) pipe-structure
	$(MAKE) disasm-cov
	$(MAKE) golden-cross

torture-setup:
	bash scripts/setup_torture.sh

torture: build torture-setup
	bash scripts/torture_test.sh all

kernel-full-build: build
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" KERNEL_DIR="$(KERNEL_DIR)" JOBS="$(JOBS)" LOG="$(LOG)" bash scripts/kernel_full_build.sh

kernel-full-check: build
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" KERNEL_DIR="$(KERNEL_DIR)" bash scripts/kernel_full_check.sh

clean-cache:
	bash scripts/cleanup_disk.sh --all

bootstrap: build
	DCC="$(DCC)" bash scripts/bootstrap.sh
