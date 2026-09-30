#!/bin/bash
# scripts/arm_mips_golden_baseline.sh - ARM/MIPS 交叉产物基线捕获与漂移校验
#
# 这个脚本曾经是「空跑还报告成功」的形态，四处缺陷叠在一起：
#   1. 三个夹具路径 (`basic/test_add.c` / `control/if_else.c` / `functions/function_call.c`)
#      在仓库里不存在，循环里 `continue` 跳过 ⇒ 实际什么都没编译；
#   2. 目标开关写成 `-m=arm`，dcc 的交叉目标开关是 `--target <triple>` ⇒ 就算夹具存在也编不出
#      对应架构的产物；
#   3. `normalize_asm` 在**定义之前**就被调用 ⇒ 一旦真编译成功就在此中断；
#   4. 基线只是每次运行重新生成一遍，没有任何「与上次比」的判据 ⇒ 漂移看不出来。
# 现在：夹具缺失/编译失败/产物与基线不一致一律置红；基线不存在时捕获并明示 CAPTURED。
#
# 判据用**目标文件字节哈希**而不是 objdump 文本：交叉 objdump 不一定装在本机，而基线要盯的
# 恰恰是「同一份输入有没有产出不同字节」。
#
# 用法: DCC=... bash scripts/arm_mips_golden_baseline.sh [capture|verify]
#   capture（默认）: 基线缺失则写入，存在则校验。
#   verify: 基线缺失即失败（CI 用，禁止悄悄重写基线）。
#
# 基线存在 `tests/golden/` 而不是 `golden_baseline/`：后者整目录被 .gitignore 排除且
# **一个文件也未入库**，所以任何存进去的基线都只是本机临时件，别人 clone 后拿不到。
# 基线要成为门禁就必须是仓库内容。
set -o pipefail
DCC=${DCC:-/root/dcc/target/release/dcc}
REPO=${DCC_REPO:-/root/dcc}
BASELINE_DIR=${BASELINE_DIR:-/root/dcc/tests/golden/arm_mips}
MODE=${1:-capture}
mkdir -p "$BASELINE_DIR"

# 目标三元组（dcc 的 --target 取值）与夹具都必须真实存在，缺失即红。
TARGETS=(arm-linux-gnueabi mips-linux-gnu)
FIXTURES=(
    "tests/fixtures/c/address_const.c"
    "tests/fixtures/c/apply_ret.c"
    "tests/fixtures/c/array_dim_const_fold.c"
)

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
fails=0
captured=0
checked=0

for t in "${TARGETS[@]}"; do
    for rel in "${FIXTURES[@]}"; do
        src="$REPO/$rel"
        name=$(basename "$src" .c)
        base="$BASELINE_DIR/${t}_${name}.sha256"
        if [ ! -f "$src" ]; then
            # 以前这里是 `continue` + 一句警告：夹具被移动/改名会让门禁永远「全绿」，
            # 而它实际什么都没测。缺文件就是缺测，必须算失败。
            echo "MISSING  $t $rel (夹具不存在)"
            fails=$((fails + 1))
            continue
        fi
        obj="$TMP/${t}_${name}.o"
        if ! "$DCC" --target "$t" -c -O0 -o "$obj" "$src" >"$obj.log" 2>&1; then
            echo "COMPILE-FAIL $t $rel"
            sed 's/^/    /' "$obj.log" | head -5
            fails=$((fails + 1))
            continue
        fi
        got=$(sha256sum "$obj" | awk '{print $1}')
        if [ ! -f "$base" ]; then
            if [ "$MODE" = "verify" ]; then
                echo "NO-BASELINE $t $rel (verify 模式不得新建基线)"
                fails=$((fails + 1))
                continue
            fi
            echo "$got  $t $rel" > "$base"
            echo "CAPTURED $t $rel <- $got"
            captured=$((captured + 1))
            continue
        fi
        want=$(awk '{print $1}' "$base")
        if [ "$want" = "$got" ]; then
            echo "OK       $t $rel"
        else
            echo "DRIFT    $t $rel"
            echo "           基线 $want"
            echo "           当前 $got"
            fails=$((fails + 1))
        fi
        checked=$((checked + 1))
    done
done

echo ""
echo "=== arm/mips 基线: 校验 $checked, 新捕获 $captured, 失败 $fails (mode=$MODE)"
if [ "$fails" -ne 0 ]; then
    echo "arm_mips_golden_baseline: FAIL"
    exit 1
fi
if [ "$checked" -eq 0 ] && [ "$captured" -eq 0 ]; then
    # 一个都没跑成就不算「通过」：那正是本脚本原来的失效形态。
    echo "arm_mips_golden_baseline: FAIL (没有校验到任何产物)"
    exit 1
fi
echo "arm_mips_golden_baseline: OK"
exit 0
