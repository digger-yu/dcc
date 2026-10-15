#!/bin/bash
# ===========================================================================
# scripts/check_fixtures_selftest.sh — 反向自测（元门禁：门禁本身有门禁）
#
# 权威规格: tests/README.md §7（元门禁）与 §1.1（Tier 0）；被检对象的规则见 tests/fixtures/README.md
# 目的: 用 5 类人为注入，逐条证明 scripts/check_fixtures.sh 的 5 条规则
#       **真的会红**（逐个 exit 1 且首个命中标签正确），而不是像旧脚本那样
#       一个文件都没扫仍打印 "✅ 所有 fixture 检查通过"。
#
# 依赖: 仅 bash + coreutils(grep/sed/awk/find/sort/comm/cut/tr) + git
# 注入载体（落点文件尚不存在时不得静默跳过 —— 跳过 = 不算 PASS）:
#   1) tests/integration/c_lang.rs 存在 → 按 §8.3 原样在它身上注入（先备份，逐例还原）
#   2) 否则 → 自建 tests/integration/zz_gate_carrier.rs（同在该目录 glob 的扫描面内，
#      非 cargo target、不被任何 include! 引用，cleanup 阶段删除）
#
# 退出码: 0 = 5/5 PASS 且无 zz 残留; 1 = 任一用例未红/标签不符/残留未清
# 用法:   bash scripts/check_fixtures_selftest.sh
# ===========================================================================
set -o pipefail
export LC_ALL=C

fail_fatal() {
    echo "FATAL: $*"
    exit 1
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)"
if [[ -z "${REPO_ROOT:-}" || ! -d "$REPO_ROOT/tests/fixtures" ]]; then
    fail_fatal "repo root 解析失败: '${REPO_ROOT:-<empty>}' 或 tests/fixtures 不存在"
fi
cd "$REPO_ROOT" || fail_fatal "无法进入仓库根目录: $REPO_ROOT"

GATE="scripts/check_fixtures.sh"
[[ -f "$GATE" ]] || fail_fatal "被测门禁不存在: $GATE（无法自测 = 失败，不作为通过）"
bash -n "$GATE" 2>/dev/null || fail_fatal "$GATE 语法检查未通过"

WORK="$(mktemp -d)" || fail_fatal "mktemp 失败"

# --- 注入点（全部带 zz/m99 标记，便于残留审计）-----------------------------
CARRIER=""                    # 运行期决定
CARRIER_CREATED=0             # 1 = 本脚本自建，须在 cleanup 删除
DIR_CREATED=0                 # 1 = tests/integration 由本脚本 mkdir，cleanup 时 rmdir
CARRIER_BAK="$WORK/carrier.bak"
CARRIER_ORIG=""               # 非空 = 落点是仓库既有文件，须逐例还原
FX_C="tests/fixtures/c"
# ① 的注入件故意不创建（只存在引用），因此不列入 INJ_* 清理集
INJ_UNREF="$FX_C/zz_unreferenced.c"
INJ_NAME="$FX_C/m99_new_thing.c"
INJ_DIR="$FX_C/zz_bad_dir.cpp"
INJ_ARCHIVE_REF='../fixtures/_archive/dev/builtins.c'   # §8.3 表第 3 行指定的归档件

cleanup() {
    rm -f "$INJ_UNREF" "$INJ_NAME" "$INJ_DIR" 2>/dev/null
    if [[ -n "$CARRIER_ORIG" && -f "$CARRIER_BAK" ]]; then
        cp -p "$CARRIER_BAK" "$CARRIER_ORIG" 2>/dev/null
    fi
    if [[ $CARRIER_CREATED -eq 1 && -n "$CARRIER" ]]; then
        rm -f "$CARRIER" 2>/dev/null
    fi
    if [[ $DIR_CREATED -eq 1 ]]; then
        rmdir tests/integration 2>/dev/null
    fi
    rm -rf "$WORK" 2>/dev/null
    return 0
}
trap cleanup EXIT
trap 'echo "interrupted" >&2; cleanup; exit 130' INT TERM

run_gate() { bash "$GATE" >"$WORK/out" 2>&1; echo "$?" >"$WORK/rc"; }

# --- 基线：注入前的门禁输出（用于证明注入项确实「新增」了违规）--------------
run_gate
BASE_RC="$(cat "$WORK/rc")"
BASE_OUT="$WORK/base.out"
cp "$WORK/out" "$BASE_OUT"
echo "=== check_fixtures 反向自测（§8.3）==="
echo "baseline: exit=$BASE_RC, $(grep -cE '^(MISSING|UNREFERENCED|FORBIDDEN-ARCHIVE-REF|FORBIDDEN-NAME|DIR-MISMATCH):' "$BASE_OUT") violation(s) already present"
# 反假绿：被检门禁必须真的扫到了源文件（scanned>0），否则自测无意义
SCANNED="$(sed -n 's/^scanned=\([0-9]\{1,\}\).*/\1/p' "$BASE_OUT" | head -1)"
if [[ -z "$SCANNED" || "$SCANNED" -le 0 ]]; then
    echo "FATAL: 门禁未报告有效 scanned 计数（'${SCANNED:-<none>}'）—— 无法证明门禁在真扫描"
    exit 1
fi
echo "gate scanned=$SCANNED source files"
if [[ "$BASE_RC" -eq 0 ]]; then
    echo "note: 基线全绿 —— 各用例的 exit 1 必须完全由注入引入"
else
    echo "note: 基线已红（存量债），断言只看『注入件是否被期望标签命中』，不放宽任何规则"
fi

# --- 载体准备 ---------------------------------------------------------------
if [[ ! -d tests/integration ]]; then
    mkdir -p tests/integration || fail_fatal "无法创建 tests/integration"
    DIR_CREATED=1
fi
if [[ -f tests/integration/c_lang.rs ]]; then
    CARRIER="tests/integration/c_lang.rs"
    CARRIER_ORIG="$CARRIER"
    cp -p "$CARRIER" "$CARRIER_BAK" || fail_fatal "备份 $CARRIER 失败"
    echo "carrier: $CARRIER (仓库既有文件, 逐例备份/还原)"
else
    CARRIER="tests/integration/zz_gate_carrier.rs"
    CARRIER_CREATED=1
    echo "carrier: $CARRIER (c_lang.rs 尚未生成 -> 自建扫描面内载体文件, 结束后删除)"
fi

reset_carrier() {
    if [[ $CARRIER_CREATED -eq 1 ]]; then
        printf '// zz injection carrier for scripts/check_fixtures_selftest.sh (deleted by cleanup)\n' >"$CARRIER"
    else
        cp -p "$CARRIER_BAK" "$CARRIER"
    fi
}
inject_line() { printf '%s\n' "$1" >>"$CARRIER"; }

# --- 断言：exit 1 + 首个命中标签正确 + 注入件确实新增 -----------------------
CASES_PASS=0
CASES_TOTAL=0
check_case() { # $1=用例名 $2=期望标签 $3=注入件定位 token
    local name="$1" label="$2" token="$3"
    CASES_TOTAL=$((CASES_TOTAL + 1))
    run_gate
    local rc; rc="$(cat "$WORK/rc")"
    local ok=1 why=""

    if [[ "$rc" -ne 1 ]]; then
        why="exit=$rc (期望 1，规则未生效)"
    fi
    if grep -qF -- "$token" "$BASE_OUT"; then
        why="${why:+$why; }注入件在基线中已出现（用例无法证明规则生效）"
    fi
    # 注入件相关的全部命中行；首行标签必须等于期望标签
    local hits first
    hits="$(grep -F -- "$token" "$WORK/out" | grep -E '^(MISSING|UNREFERENCED|FORBIDDEN-ARCHIVE-REF|FORBIDDEN-NAME|DIR-MISMATCH):' || true)"
    if [[ -z "$hits" ]]; then
        why="${why:+$why; }输出中无含注入件的违规行"
    else
        first="${hits%%$'\n'*}"
        first="${first%%:*}"
        if [[ "$first" != "$label" ]]; then
            why="${why:+$why; }首个命中标签为 $first:（期望 $label:）"
        fi
    fi
    [[ -n "$why" ]] && ok=0

    if [[ $ok -eq 1 ]]; then
        CASES_PASS=$((CASES_PASS + 1))
        printf '[%d/5] %-22s PASS  exit=1  first_label=%s:\n' "$CASES_TOTAL" "$name" "$label"
        printf '       %s\n' "${hits%%$'\n'*}"
    else
        printf '[%d/5] %-22s FAIL %s\n' "$CASES_TOTAL" "$name" "$why"
        printf '       实际命中行: %s\n' "${hits:-<none>}"
    fi
}

# ── ① MISSING ──────────────────────────────────────────────────────────────
reset_carrier
inject_line 'const ZZ_GATE_MISSING: &str = include_str!("../fixtures/c/zz_nonexistent.c");'
check_case "MISSING" "MISSING" "zz_nonexistent.c"

# ── ② UNREFERENCED ─────────────────────────────────────────────────────────
reset_carrier
printf 'int zz_unreferenced(void) { return 0; }\n' >"$INJ_UNREF"
check_case "UNREFERENCED" "UNREFERENCED" "zz_unreferenced.c"
rm -f "$INJ_UNREF"

# ── ③ FORBIDDEN-ARCHIVE-REF ────────────────────────────────────────────────
reset_carrier
if [[ ! -f tests/fixtures/_archive/dev/builtins.c ]]; then
    echo "[3/5] FORBIDDEN-ARCHIVE-REF 目标件 tests/fixtures/_archive/dev/builtins.c 缺失 -> 仍执行注入（不允许静默跳过）"
fi
inject_line 'const ZZ_GATE_ARCHIVE: &str = include_str!("'"$INJ_ARCHIVE_REF"'");'
check_case "ARCHIVE-REF" "FORBIDDEN-ARCHIVE-REF" "_archive/dev/builtins.c"

# ── ④ FORBIDDEN-NAME ───────────────────────────────────────────────────────
reset_carrier
printf 'int m99_new_thing(void) { return 0; }\n' >"$INJ_NAME"
check_case "FORBIDDEN-NAME" "FORBIDDEN-NAME" "m99_new_thing.c"
rm -f "$INJ_NAME"

# ── ⑤ DIR-MISMATCH ────────────────────────────────────────────────────────
reset_carrier
SRC_CPP="tests/fixtures/cpp/auto_deduce.cpp"
if [[ ! -f "$SRC_CPP" ]]; then
    SRC_CPP="$(find tests/fixtures/cpp -maxdepth 1 -name '*.cpp' -type f 2>/dev/null | sort | head -1)"
    [[ -n "$SRC_CPP" ]] || SRC_CPP="tests/fixtures/cpp/auto_deduce.cpp"   # 不存在也照抄，用 cp 失败暴露问题
fi
if ! cp "$SRC_CPP" "$INJ_DIR" 2>/dev/null; then
    echo "FATAL: 无法从 $SRC_CPP 复制出 DIR-MISMATCH 注入件（cpp/ 目录无可用样例）"
    CASES_TOTAL=$((CASES_TOTAL + 1))
else
    check_case "DIR-MISMATCH" "DIR-MISMATCH" "zz_bad_dir.cpp"
    rm -f "$INJ_DIR"
fi

reset_carrier
if [[ $CARRIER_CREATED -eq 1 ]]; then
    rm -f "$CARRIER"
else
    cp -p "$CARRIER_BAK" "$CARRIER"
fi

# --- 残留审计：不得留下任何本次注入痕迹 -------------------------------------
# 锁定到本次注入的具体名字（而不是子串 'zz'），避免误伤含 zz 的合法夹具名
# （如 buzz_light.c）；carrier 为自建文件时额外做文件系统存在件检查。
echo "---"
RESIDUAL_RE='zz_nonexistent|zz_unreferenced|zz_bad_dir|zz_gate_carrier|m99_new_thing'
RESIDUAL="$(git status --porcelain --untracked-files=all tests/fixtures tests/integration 2>/dev/null \
    | grep -E "$RESIDUAL_RE" || true)"
for f in "$INJ_UNREF" "$INJ_NAME" "$INJ_DIR"; do
    [[ -e "$f" ]] && RESIDUAL="$RESIDUAL
filesystem: $f still exists"
done
if [[ $CARRIER_CREATED -eq 1 && -e "$CARRIER" ]]; then
    RESIDUAL="$RESIDUAL
filesystem: $CARRIER still exists"
fi
if [[ -n "$RESIDUAL" ]]; then
    echo "RESIDUE: 注入痕迹未清除:"
    echo "$RESIDUAL"
    echo "$CASES_PASS/$CASES_TOTAL PASS（但因存在残留，判定失败）"
    exit 1
fi
echo "residue: none (git status --porcelain --untracked-files=all tests/fixtures tests/integration 无注入痕迹)"

echo "---"
echo "$CASES_PASS/$CASES_TOTAL PASS"
if [[ $CASES_PASS -eq 5 && $CASES_TOTAL -eq 5 ]]; then
    echo "selftest passed: 5 条门禁规则均被证明真的会红。"
    exit 0
fi
echo "selftest FAILED: 期望 5/5 PASS，实际 $CASES_PASS/$CASES_TOTAL"
exit 1
