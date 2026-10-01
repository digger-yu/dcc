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
#   2) cargo test --release  —— 6 个 target 全部测试通过
#   3) check_fixtures.sh     —— fixture 门禁本体
#   4) check_fixtures_selftest.sh —— 元门禁（证明 3 真的会红）
# 失败即红 —— 不设 `|| true`，不做任何降级。
test:
	cargo build --tests
	cargo test --release
	DCC_REPO="$(DCC_REPO)" bash scripts/check_fixtures.sh
	DCC_REPO="$(DCC_REPO)" bash scripts/check_fixtures_selftest.sh

# test-all = Tier 0 全量 + examples 端到端冒烟（scripts/test_all.sh 只做薄编排，
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

test-all: test
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" bash scripts/test_all.sh
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" JOBS="$(JOBS)" bash scripts/check_opt_equivalence.sh
	$(MAKE) rex-hygiene
	$(MAKE) pipe-structure
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
