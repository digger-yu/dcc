.PHONY: all build test test-all bootstrap kernel-full-build kernel-full-check clean-cache torture-setup torture

DCC_REPO ?= $(CURDIR)
KERNEL_DIR ?= /root/linux-7.1
DCC ?= $(DCC_REPO)/target/release/dcc
JOBS ?= $(shell nproc 2>/dev/null || echo 4)
LOG ?= $(DCC_REPO)/build.log

all: build test-all kernel-full-build

build:
	cargo build --release

test:
	cargo test --release

test-all: test
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" bash scripts/check_fixtures.sh

torture-setup:
	bash scripts/setup_torture.sh

torture: torture-setup build
	cargo test --release -- torture

kernel-full-build: build
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" KERNEL_DIR="$(KERNEL_DIR)" JOBS="$(JOBS)" LOG="$(LOG)" bash scripts/kernel_full_build.sh

kernel-full-check: build
	DCC="$(DCC)" DCC_REPO="$(DCC_REPO)" KERNEL_DIR="$(KERNEL_DIR)" bash scripts/kernel_full_check.sh

clean-cache:
	bash scripts/cleanup_disk.sh --all

bootstrap: build
	DCC="$(DCC)" bash scripts/bootstrap.sh
