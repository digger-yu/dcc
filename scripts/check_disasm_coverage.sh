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
set -o pipefail
for a in "$@"; do
    case "$a" in
        LEVELS=*|CEILING=*|DCC=*|DCC_REPO=*|JOBS=*) export "${a%%=*}=${a#*=}";;
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
export DCC REPO

total_unknown=0
total_instr=0
residues=()
# 单个夹具×档位的活直接写在下面 xargs 的 `bash -c` 里（并行下需要自己开临时文件，
# 而 residue / counts 按档位聚合到主进程持有的两个文件）。两条约定：
#   * 编译不过的夹具计 0 —— 那是 opt-eq 的范围，混进来只会让两个读数互相污染；
#   * `-S` 的指令行是制表符缩进，伪指令（.text/.file/.globl）与标签行都不计入分母。
for lvl in $LEVELS; do
    rf="$TMP/residue.$lvl"
    : > "$rf"
    cf="$TMP/counts.$lvl"
    : > "$cf"
    find "$REPO/tests/fixtures/c" "$REPO/tests/fixtures/opt" -name '*.c' | sort \
      | xargs -P "$jobs" -I@ bash -c '
            f=$1; lvl=$2; rf=$3
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
            echo "$t $u"
            rm -f "$out"
        ' _ @ "$lvl" "$rf" >> "$cf"
    read -r instr unk < <(awk "{t+=\$1; u+=\$2} END{printf \"%d %d\\n\", t+0, u+0}" "$cf")
    total_instr=$((total_instr + instr))
    total_unknown=$((total_unknown + unk))
    residues+=("$rf")
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

ceiling=${CEILING:-$total_unknown}
if [ "$total_unknown" -gt "$ceiling" ]; then
    echo "check_disasm_coverage: FAIL（读不出 $total_unknown > 上限 $ceiling）"
    exit 1
fi
if [ "$total_unknown" -lt "$ceiling" ]; then
    echo "注意: 比上限少了 $((ceiling - total_unknown)) 条 —— 请把 CEILING 降到 $total_unknown，别把余量留给以后的退化"
fi
echo "check_disasm_coverage: OK（${pct}%，上限 $ceiling）"
