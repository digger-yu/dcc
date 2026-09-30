.PHONY: all build test test-all opt-eq bootstrap kernel-full-build kernel-full-check clean-cache torture-setup torture

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

test-all: test
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" bash scripts/test_all.sh
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" JOBS="$(JOBS)" bash scripts/check_opt_equivalence.sh

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
