#!/bin/bash
# scripts/check_disasm_coverage.sh - 自家反汇编读数（`-S`）的逐字命中率
#
# 为什么需要它：`# UNKNOWN` 是反汇编器的兜底出口，它**不影响产物**，所以没有任何
# 现有用例会因它变多而变红。但这份读数不是装饰：`rex_matrix` 那套穷举矩阵判的是
# 「编出来再读回来，名字必须恰好是意图的那几个」，读不出来的地方判据就没有读数
# （§7.23 为此专门把「解码失败」也判红）。没有这条度量，兜底出口会悄悄变多，
# 覆盖面就成了假的。
#
# 判据用**绝对条数上限**而不是百分比：百分比会随语料涨跌自动放宽/收紧标准，
# 条数上限只有「变好就调低、变差就红」两种动作。报告还会把残留按字节序列聚合
# 打出来 —— 那就是下一轮的待补清单（这里过去只有一句「≈3%」，没人能据此开工）。
#
# 用法: DCC=... DCC_REPO=... LEVELS="0 2" CEILING=106 bash scripts/check_disasm_coverage.sh
#   也支持位置参数形式 `LEVELS="0 2"`（只读环境变量时会把传参静默忽略，看起来像在跑
#   其实只跑了默认档位 —— 又是一类“指标谎报”）。
#   LEVELS   空格分隔的档位列表（默认 -O2）
#   CEILING  允许的读不出总条数；不设置时等于「本次实测值」，即只做自述不断言
#   SKEW_ASSERT=1 把「每函数指令条数不一致」也计入断言（默认 0 = 只报告，理由见下面
#                错位探测器的注释）
set -o pipefail
for a in "$@"; do
    case "$a" in
        LEVELS=*|CEILING=*|DCC=*|DCC_REPO=*|JOBS=*|SKEW_ASSERT=*) export "${a%%=*}=${a#*=}";;
        *) echo "用法: $0 [LEVELS='0 2'] [CEILING=n]（未知参数: $a）" >&2; exit 2;;
    esac
done
DCC=${DCC:-/root/dcc/target/release/dcc}
REPO=${DCC_REPO:-/root/dcc}
LEVELS=${LEVELS:-2}
jobs=${JOBS:-2}
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

if [ ! -x "$DCC" ]; then
    echo "DISASM-COV FAIL: 找不到可执行的 dcc: $DCC"
    exit 1
fi

# xargs 起的 `bash -c` 子进程只继承**导出**的变量。上一版少了这一行时 $cf/$rf 在子
# 进程里是空串，`>> ""` 全部失败，最后报「指令行 0、读不出 0、OK」—— 那是典型的
# 假绿：度量坏成空转反而会通过。
export DCC REPO TMP

# ---------------------------------------------------------------------------
# 错位探测器为什么必须存在（与上面的 UNKNOWN 上限是两条正交的判据）：
# `# UNKNOWN` 只数「读不出来」的那几条。而指令流**错位**之后读出来的是有名字的
# 假指令 —— 本轮实测：MOVSXD 把 REX.B 双重施加到 rm，`rm==4`(SIB) 变成 `12` ⇒
# 既不读 SIB 也不读位移 ⇒ 长度少算一个字节 ⇒ 后面整条流被重读成
# `fmuls 99(%rcx)`、`mov -117(%rax)`、`movsxd (%r12)` 这种「看着合法」的行，
# UNKNOWN 计数一条都不报。同一个函数，dcc 自己念出 N 条、objdump 在真实字节上念出
# M 条，N≠M 就是长度不对最便宜的捕手（代价：每个夹具多一次 `-c` 编译 + 一次 objdump）。
#
# 比较的键集取 objdump 认得的函数（symtab 里的那些）；只在单边出现的名字单独归类为
# 「边界差异」（objdump 与 dcc 对函数边界的定义本来可以不同，例如对齐填充、thunk），
# 不混进条数比较，免得把真 skew 淹在噪声里。
# ---------------------------------------------------------------------------
cat > "$TMP/od.awk" <<'AWK'
# objdump -d 的每符号指令条数（入参必须是 `objdump -d --no-show-raw-insn`）。
# 四条固定规则，少一条就会造出假 skew：
#   * 不能带机器码列：长指令的字节列会**换行**（实测 `48 b8 11 00 00 00 00` 后接
#     一行 `  23:\t00 00 00`），那一行同样是「偏移+冒号」的形状，会被多计一条；
#   * 符号头 `0000...<name>:`；名字里带 `+`（函数内部符号）不改当前函数，否则
#     父函数的条数会从中间剖开，产生假 skew；
#   * `Disassembly of section ...` 之后当前符号失效（换节不串数）；
#   * 指令行是「偏移 + 冒号 + 制表符 + 非空」，漏掉助记词的行（纯偏移）不计数；
#   * `nop` 不计数：objdump 是按 symtab 里的 st_size 逐字节读，函数**尾部**对齐填充
#     会被归到前一个函数名下，而 dcc 的文本只写到函数逻辑末尾（实测：某夹具每个
#     函数尾部多 7 条 nop，一度把 152/188 个函数报成 skew）。错位捕手不依赖 nop：
#     错位读出来的是有名字的假指令（`fmuls 99(%rcx)`、`movsxd (%r12)`），不是 nop；
#     语料里填充全是单字节 `nop`（无 data16/nopw 形），所以两边同时跳过它是对称的。
/^Disassembly of section/ { cur = ""; next }
/^[0-9a-f]+ <.*>:$/ {
    n = $0; sub(/^.*</, "", n); sub(/>:$/, "", n)
    if (n ~ /\+/) next
    cur = n; next
}
/^[ \t]+[0-9a-f]+:[ \t]+[^ \t]/ {
    if (cur == "") next
    m = $0
    sub(/^[ \t]+[0-9a-f]+:[ \t]+/, "", m)
    sub(/[ \t].*$/, "", m)
    if (m == "nop") next
    c[cur]++
    next
}
END { for (k in c) print k, c[k] }
AWK

cat > "$TMP/dc.awk" <<'AWK'
# dcc -S 文本的每函数指令条数，键集只取 objdump 认得的那些（-v OD=<名单文件>）。
# 与分母那三条约定一致：指令行是制表符缩进；伪指令（`.text`/`.globl`/`.type`/
# `.size`/`.byte`…）与注释不算指令；第 0 列以冒号收尾的是标号，其中 `.L*` 是局部
# 标号（循环/分支标签）不是函数边界，不得拿它重置当前函数。`nop` 也不计数，理由
# 与 od.awk 那一条完全对称（尾部对齐填充归 objdump 的符号 size，不归这份文本）。
BEGIN { while ((getline l < OD) > 0) { split(l, a, " "); sel[a[1]] = 1 } }
{
    if (substr($0, 1, 1) == "\t") {
        d = substr($0, 2, 1)
        if (d == "." || d == "#") next
        if (cur != "" && $1 != "nop") c[cur]++
        next
    }
    line = $0
    if (line ~ /:[ \t]*$/) {
        sub(/:[ \t]*$/, "", line)
        if (line ~ /^\.L/) next
        cur = (line in sel) ? line : ""
        next
    }
}
END { for (k in c) print k, c[k] }
AWK

cat > "$TMP/sk.awk" <<'AWK'
# 两边条数对起来。输出七列：类别 |差| 档位 文件 函数 dcc条数 objdump条数。
#   EQ       两边一致（分母）
#   SKEW     两边不一致 —— 错位嫌疑，本次要报的门
#   ONLYOBJ  objdump 有、dcc 文本里没有那个函数标号（边界差异，不当 skew）
#   ONLYDCC  dcc 文本里有、objdump 不认（常见于 static/未进 symtab 的函数）
NR == FNR { o[$1] = $2 + 0; on[$1] = 1; next }
{ d[$1] = $2 + 0; dn[$1] = 1 }
END {
    for (k in on) {
        if (k in dn) {
            diff = d[k] - o[k]; if (diff < 0) diff = -diff
            printf "%s %d %s %s %s %d %d\n", (diff ? "SKEW" : "EQ"), diff, LVL, FN, k, d[k], o[k]
        } else {
            printf "ONLYOBJ %d %s %s %s 0 %d\n", o[k], LVL, FN, k, o[k]
        }
    }
    for (k in dn)
        if (!(k in on)) printf "ONLYDCC %d %s %s %s %d 0\n", d[k], LVL, FN, k, d[k]
}
AWK

total_unknown=0
total_instr=0
residues=()
skew_files=()
# 单个夹具×档位的活直接写在下面 xargs 的 `bash -c` 里（并行下需要自己开临时文件，
# 而 residue / counts 按档位聚合到主进程持有的两个文件）。两条约定：
#   * 编译不过的夹具计 0 —— 那是 opt-eq 的范围，混进来只会让两个读数互相污染；
#   * `-S` 的指令行是制表符缩进，伪指令（.text/.file/.globl）与标签行都不计入分母。
for lvl in $LEVELS; do
    rf="$TMP/residue.$lvl"
    : > "$rf"
    cf="$TMP/counts.$lvl"
    : > "$cf"
    sf="$TMP/skew.$lvl"
    : > "$sf"
    find "$REPO/tests/fixtures/c" "$REPO/tests/fixtures/opt" -name '*.c' | sort \
      | xargs -P "$jobs" -I@ bash -c '
            f=$1; lvl=$2; rf=$3; sf=$4
            out=$(mktemp)
            if ! "$DCC" -O"$lvl" -std=gnu2x -I "$REPO/tests/fixtures/c" -I "$REPO" \
                    -S -o "$out" "$f" >/dev/null 2>&1; then
                echo "0 0"; rm -f "$out"; exit 0
            fi
            # 指令行 = 制表符开头、不是伪指令（.text/.file/.globl）、不以冒号结尾（标签）
            t=$(grep -P "^\t" "$out" | grep -vP "^\t\." | grep -vP ":[[:space:]]*$" | wc -l)
            u=$(grep -c "# UNKNOWN" "$out" || true)
            if [ "$u" -gt 0 ]; then
                grep "# UNKNOWN" "$out" | sed -E "s/^[ \t]*# UNKNOWN //" >> "$rf"
            fi
            # 错位探测：同一份源码、同一档位再编一次成对象，用 objdump 在**真实字节**
            # 上数每函数条数，与上面那份文本对账。`-c` 挂掉的夹具归 0（与上面同一条
            # 约定：编译不过不是解码器的锅，不得让它污染 skew 读数）。
            obj=$(mktemp)
            if "$DCC" -O"$lvl" -std=gnu2x -I "$REPO/tests/fixtures/c" -I "$REPO" \
                    -c -o "$obj" "$f" >/dev/null 2>&1 && [ -s "$obj" ]; then
                od=$(mktemp); dc=$(mktemp)
                objdump -d --no-show-raw-insn "$obj" 2>/dev/null | awk -f "$TMP/od.awk" > "$od"
                awk -v OD="$od" -f "$TMP/dc.awk" "$out" > "$dc"
                awk -v LVL="$lvl" -v FN="$(basename "$f")" -f "$TMP/sk.awk" "$od" "$dc" >> "$sf"
                rm -f "$od" "$dc"
            fi
            rm -f "$obj"
            echo "$t $u"
            rm -f "$out"
        ' _ @ "$lvl" "$rf" "$sf" >> "$cf"
    read -r instr unk < <(awk "{t+=\$1; u+=\$2} END{printf \"%d %d\\n\", t+0, u+0}" "$cf")
    total_instr=$((total_instr + instr))
    total_unknown=$((total_unknown + unk))
    residues+=("$rf")
    skew_files+=("$sf")
    printf '档位 -O%s: 指令行 %s, 读不出 %s\n' "$lvl" "$instr" "$unk"
done

pct=$(awk -v a="$total_unknown" -v b="$total_instr" 'BEGIN{printf "%.4f", (b? a*100/b : 0)}')
echo "=== 合计: 指令行 $total_instr, 读不出 $total_unknown, 占比 ${pct}%"

# 分母为 0 一定不是「零缺陷」而是「度量坏了」（计数正则、导出变量、-S 格式变动
# 都会造成这个形状），所以直接判红，不让它以 0/0 通过。
if [ "$total_instr" -eq 0 ]; then
    echo "check_disasm_coverage: FAIL（指令行分母为 0：度量本身坏了，不是零缺陷）"
    exit 1
fi

if [ "$total_unknown" -gt 0 ]; then
    echo "=== 残留按字节序列聚合（即待补清单；名字拿不准的一律报 UNKNOWN 而不是猜）==="
    cat "${residues[@]}" 2>/dev/null | sort | uniq -c | sort -rn | head -15
fi

# ---------------- 错位探测结果 ----------------
skew_all="$TMP/skew.all"
cat "${skew_files[@]}" 2>/dev/null > "$skew_all"
skew_cmp=$(awk '$1 == "EQ" || $1 == "SKEW" { n++ } END { print n+0 }' "$skew_all")
skew_bad=$(awk '$1 == "SKEW" { n++ } END { print n+0 }' "$skew_all")
skew_edge=$(awk '$1 == "ONLYOBJ" || $1 == "ONLYDCC" { n++ } END { print n+0 }' "$skew_all")
echo "=== 错位探测: 每函数条数比较 $skew_cmp 个, 不一致 $skew_bad 个, 边界单边 $skew_edge 个"
if [ "$skew_bad" -gt 0 ]; then
    echo "--- 条数不一致的函数按差异从大到小前 10（dcc -S 读数 vs objdump 读数）---"
    awk '$1 == "SKEW"' "$skew_all" | sort -k2,2nr -k4,4 -k5,5 | head -10 \
      | awk '{ printf "  O%-2s %-30s %-26s dcc=%s objdump=%s 差=%s\n", $3, $4, $5, $6, $7, $2 }'
fi
if [ "$skew_edge" -gt 0 ]; then
    echo "--- 函数边界单边（objdump 只列 symtab 里的函数；不是 skew，只供定性）---"
    awk '$1 == "ONLYOBJ" || $1 == "ONLYDCC"' "$skew_all" | sort | head -5
fi

# 分母为 0 一定不是「零错位」而是探测器自己坏了（objdump 不在 PATH、`-c` 全挂、
# awk 脚本名写错都会造成这个形状）—— 与上面指令行分母那一条是同一个先例、同一句理由。
if [ "$skew_cmp" -eq 0 ]; then
    echo "check_disasm_coverage: FAIL（错位探测分母为 0：探测器本身坏了，不是零错位）"
    exit 1
fi

# 为什么默认只报告不断言（实测数字，2026-10-02，全语料 LEVELS="0 2"）：本次实测
# 2470 个函数参与条数比较、16 个不一致、25 个边界单边。那 16 条逐条做过三分定性
#（dc = `dcc -S` 文本自数的条数；as = 那份文本被 GAS 回炸后 objdump 数的条数；
# obj = `dcc -c` 直发对象 objdump 数的条数，清单见 docs/wp_disasm.md）：
#   * 14 条 dc == as ≠ obj —— 那份文本自身是自洽的（回炸后一条不差），差在 dcc 的
#     `-S` 与 `-c` 两条产出路径**发的不是同一串代码**（典型：static_local_arrays.c
#     四个函数、static_agg_by_value.c main 593/625、builtin_misc.c test_jmp 44/16）；
#   * 2 条是 sched_rq_lock_patterns.c 的 raw_spin_lock，`as` 在它身上终止（`lock mov
#     $0, (%r12)` → `expecting lockable instruction after 'lock'`），根因也在 `-S`
#     路径发射的字节上（objdump 在 `-c` 对象里读作 `lock movl $0x0,(%r12)`）。
# 两者都不是解码器错位。把这条门断言上去只会把别人的产出路径差异记在本文件名下，
# 所以保持 SKEW_ASSERT=0（只报告）。等 `-S`/`-c` 两条路径对齐、skew 回到 0，再把
# 默认值改成 1，并在这里写下当时实测的条数。
skew_assert=${SKEW_ASSERT:-0}
if [ "$skew_bad" -gt 0 ]; then
    if [ "$skew_assert" -eq 1 ]; then
        echo "check_disasm_coverage: FAIL（每函数条数不一致 $skew_bad > 0，且 SKEW_ASSERT=1）"
        exit 1
    fi
    echo "注意: 错位探测发现 $skew_bad 个函数条数不一致，但它还没进断言（SKEW_ASSERT=0）"
fi

ceiling=${CEILING:-$total_unknown}
if [ "$total_unknown" -gt "$ceiling" ]; then
    echo "check_disasm_coverage: FAIL（读不出 $total_unknown > 上限 $ceiling）"
    exit 1
fi
if [ "$total_unknown" -lt "$ceiling" ]; then
    echo "注意: 比上限少了 $((ceiling - total_unknown)) 条 —— 请把 CEILING 降到 $total_unknown，别把余量留给以后的退化"
fi
echo "check_disasm_coverage: OK（${pct}%，上限 $ceiling）"
