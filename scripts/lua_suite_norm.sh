#!/bin/bash
# ===========================================================================
# scripts/lua_suite_norm.sh — Lua 官方测试套件输出：噪声归一化对拍器（纯 bash）
#
# 为什么需要它
#   all.lua 的产物**不是**逐字节确定的：随机种子、pid、临时路径、耗时、GC 计数与
#   内存统计、进度点（io.write('.') 的 flush 边界）每次运行都会变。直接 diff 两棵
#   树的输出必然满屏红，谁也说不清里面有没有一条真差异。
#   本工具把「已知会变」的行按**类别**改写成占位符，然后再逐行比对。两条判据：
#     1) 只有下面 RULE_NAMES 列出的类别会被改写，每类都带取值校验（短路边号必须
#        ∈{0,1}、随机浮点区间必须 0≤lo≤hi≤1、日期必须成形）。取值超出定义域 =
#        报错退出 2，不静默吞掉 —— 否则归一化器本身就成了新的假绿源。
#     2) 改写后的两棵树必须**逐行相同**。任何未归类的差异都保留在 diff 里。
#
# 为什么是 bash 而不是 awk/python
#   规则要按类别取**捕获组**再校验。本机 awk 是 mawk 1.3.4，没有 gawk 的
#   match(s,re,arr) 三参捕获；把同一套规则在 gawk/mawk 两边写两遍等于没有真源。
#   bash>=4 的 [[ =~ ]] 自带 BASH_REMATCH 捕获组，加上 diff 就是全部依赖。
#   代价：ERE 里没有 \d/\S，且 \( \. 之类转义的可移植性差，所以模式统一写成
#   [0-9]、[^[:space:]]、[(]、[.]、[[]、[]] 这种括号形式。
#
# 用法
#   lua_suite_norm.sh REF_LOG CAND_LOG [--audit] [--max-diff N]
#   lua_suite_norm.sh --selftest
#
# 退出码：0 = 归一化后相同；1 = 有差异；2 = 用法错误或噪声取值越界。
#
# 与门禁的关系：scripts/check_lua_suite_diff.sh 负责建树、跑套件、比结构，本文件
# 只负责「两份日志归一化后是否逐行相同」这一条判据；两边都带自测，其中
# --selftest 的一半用例是**必须判红**的真差异（防止归一化规则写宽把语义差异吞掉）。
# ===========================================================================
set -o pipefail

SELF_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# ---- 规则表：类别名 + ERE（整行匹配的规则自己写 ^...$）---------------------
RULE_NAMES=(
  seed-header
  seed-tables
  short-circuit-branch
  time-per-file
  time-total
  timestamp
  progname
  tmp-prog-file
  prog-prefix
  pid
  gc-counts
  gc-counts-2
  gc-memory
  gc-memory-plain
  rand-float-range
  rand-int-range
  sort-timing
  sort-invert
  sort-equal
  mainlua-kills
)

RULE_RES=(
  '^random seeds: (-?[0-9]+), (-?[0-9]+)$'
  '[(]seeds 0X([0-9a-fA-F]+):([0-9a-fA-F]+)[)]'
  '^testing short-circuit optimizations [(]([0-9]+)[)]$'
  '^time: ([0-9.eE+-]+) [(]([0-9.eE+-]+)[)]$'
  '^total time: ([0-9.]+)s [(]wall time: ([0-9.]+)s[)]$'
  '^test done on ([0-9]+)/([0-9]+)/([0-9]+), at ([0-9]+):([0-9]+):([0-9]+)$'
  '^progname: (/[^[:space:]]+)$'
  '^[(]temporary program file used in these tests: ([^[:space:]]+)[)]$'
  '^([^[:space:]]*/)?(lua|luac): (.*)$'
  '[(]if test fails now, it may leave a Lua script running in background, pid ([0-9]+)[)]'
  $'^\t[(]strings: *([0-9]+), tables: *([0-9]+), functions: *([0-9]+), *$'
  $'^\tudata: *([0-9]+), threads: *([0-9]+)[)]$'
  '^    ---- total memory: ([^[:space:]]+) [(][^[:space:]]+[)], max use: ([^[:space:]]+),  blocks: ([0-9]+)$'
  '^    ---- total memory: ([^[:space:]]+), max memory: ([^[:space:]]+) ----$'
  '^float random range in ([0-9]+) calls: [[]([0-9.eE+-]+), ([0-9.eE+-]+)[]]$'
  '^integer random range in ([0-9]+) calls: [[]minint [+] ([0-9]+)ppm, maxint - ([0-9]+)ppm[]]$'
  '^(sorting|re-sorting) ([0-9]+) (random|sorted) elements in [0-9.]+ msec[.]$'
  '^Invert-sorting other ([0-9]+) elements in [0-9.]+ msec[.][,] with ([0-9]+) comparisons$'
  '^sorting ([0-9]+) equal elements in [0-9.]+ msec[.]$'
  # main.lua 的 SIGINT 用例：在…个不可打断的长搜索期间不停发 INT，直到 kill 失败，
  # 报的是“成功发出了几次信号”。解释器越慢，循环越早结束 ⇒ 次数越小；上游自己只断言
  # `assert(n >= 2)`（main.lua:595），这个数字从来不是语义结论。校验把同一个下限留下：
  # 小于 2 就是“信号没送到”，必须当成差异而不是噪声。
  '^done [(]with ([0-9]+) kills[)]$'
)

# 进度点：all.lua/gc.lua 用 io.write('.')（以及 gc.lua 的 '+'）打点，重定向到文件后
# flush 边界取决于缓冲区实现，同一串点会落到不同行首/行尾。处理方式：整行删掉所有
# '.' 字符，再丢弃随之变空的行。点的**总数**由 check_lua_suite_diff.sh 单独比较
# （数量对不上说明有测试没跑）。带小数的行都已在上面的规则里归一化，所以这里丢掉
# 的不可能是语义差异。

NORM_ERR=""
REPL=""

err_out() { # $1=类别 $2=原因 $3=原始行
  NORM_ERR="噪声类别 $1 取值越界: $2
  行: $3"
  return 2
}

is_float() { # 至多一个小数点 + 可选指数（[0-9.]+ 这类宽松模式靠它兜底）
  [[ $1 =~ ^[-+]?([0-9]+([.][0-9]*)?|[.][0-9]+)([eE][-+]?[0-9]+)?$ ]]
}

is_uint() { [[ $1 =~ ^[0-9]+$ ]]; }

# ---- 规则应用：按类别生成占位文本并做取值校验 -----------------------------
# 结果写全局 REPL；校验不过返回 2 并填 NORM_ERR。BASH_REMATCH 在函数里会被
# 后续 [[ =~ ]] 覆盖，所以先把 g0..g6 拷进局部变量。
rule_replace() { # $1=类别名 $2=原始行
  local name=$1 line=$2
  local g0=${BASH_REMATCH[0]} g1=${BASH_REMATCH[1]:-} g2=${BASH_REMATCH[2]:-}
  local g3=${BASH_REMATCH[3]:-} g4=${BASH_REMATCH[4]:-} g5=${BASH_REMATCH[5]:-}
  local g6=${BASH_REMATCH[6]:-}
  NORM_ERR=""

  case $name in
  seed-header)
    REPL='random seeds: <SEED>, <SEED>'
    ;;
  seed-tables)
    ((${#g1} >= 4 && ${#g2} >= 4)) || err_out "$name" "种子十六进制过短: $g1:$g2" "$line"
    REPL='(seeds <HEX>:<HEX>)'
    ;;
  short-circuit-branch)
    [[ $g1 == 0 || $g1 == 1 ]] || err_out "$name" "短路边号 $g1 不是 0/1" "$line"
    REPL='testing short-circuit optimizations (<BRANCH>)'
    ;;
  time-per-file)
    is_float "$g1" && is_float "$g2" || err_out "$name" "耗时不是合法浮点: $g1 / $g2" "$line"
    REPL='time: <T>'
    ;;
  time-total)
    is_float "$g1" && is_float "$g2" || err_out "$name" "总耗时不是合法浮点: $g1 / $g2" "$line"
    REPL='total time: <T>s (wall time: <T>s)'
    ;;
  timestamp)
    local d=$((10#$g1)) mo=$((10#$g2)) y=$((10#$g3))
    local hh=$((10#$g4)) mm=$((10#$g5)) ss=$((10#$g6))
    if ((d < 1 || d > 31 || mo < 1 || mo > 12 || y < 2000 || y > 2999 ||
      hh > 23 || mm > 59 || ss > 60)); then
      err_out "$name" "日期/时间非法: $g1/$g2/$g3 $g4:$g5:$g6" "$line"
    fi
    REPL='test done on <DATE>, at <TIME>'
    ;;
  progname)
    REPL='progname: <PATH>'
    ;;
  tmp-prog-file)
    REPL='(temporary program file used in these tests: <PATH>)'
    ;;
  prog-prefix)
    # 报错信息以解释器路径开头（"<build>/lua: main.lua:67: exit"），构建目录天然
    # 不同；只抹掉路径前缀，消息正文与行号仍然参与比对。
    REPL="<PROG>: $g3"
    ;;
  pid)
    is_uint "$g1" || err_out "$name" "pid 不是十进制: $g1" "$line"
    ((10#$g1 >= 1 && 10#$g1 <= 1073741824)) ||
      err_out "$name" "pid 超出可用范围: $g1" "$line"
    REPL='(if test fails now, it may leave a Lua script running in background, pid <PID>)'
    ;;
  gc-counts)
    is_uint "$g1" && is_uint "$g2" && is_uint "$g3" ||
      err_out "$name" "GC 计数不是十进制: $g1/$g2/$g3" "$line"
    REPL=$'\t'"(strings: <N>, tables: <N>, functions: <N>,"
    ;;
  gc-counts-2)
    is_uint "$g1" && is_uint "$g2" ||
      err_out "$name" "GC 计数不是十进制: $g1/$g2" "$line"
    REPL=$'\t'"udata: <N>, threads: <N>)"
    ;;
  gc-memory)
    is_uint "$g3" || err_out "$name" "blocks 不是十进制: $g3" "$line"
    REPL='    ---- total memory: <MEM>, max use: <MEM>,  blocks: <N>'
    ;;
  gc-memory-plain)
    # 非 ltests 变体只 print collectgarbage("count")，既没有 T.checkmemory() 也没有
    # 逐块核对。这个读数取决于字符串表/表的增长顺序与指针哈希，同一棵 gcc 树跑两遍
    # 都会差（实测 458K/459K、2732K/3678K），是纯噪声。ltests 变体的内存正确性由
    # T.checkmemory() 那一档负责，所以这里整行抹成常量不损失判据。
    REPL='    ---- total memory: <MEM>, max memory: <MEM> ----'
    ;;
  rand-float-range)
    is_uint "$g1" || err_out "$name" "调用次数不是十进制: $g1" "$line"
    is_float "$g2" && is_float "$g3" ||
      err_out "$name" "随机浮点区间端点非法: $g2/$g3" "$line"
    # 0 ≤ lo ≤ hi ≤ 1：bash 没有浮点比较，交给 awk 做一次判定
    awk -v lo="$g2" -v hi="$g3" 'BEGIN { exit !(lo + 0 >= 0 && hi + 0 <= 1 && lo + 0 <= hi + 0) }' ||
      err_out "$name" "随机浮点区间非法: [$g2, $g3]" "$line"
    REPL='float random range in <N> calls: [<R>, <R>]'
    ;;
  rand-int-range)
    local i
    for i in g1 g2 g3; do
      is_uint "${!i}" || err_out "$name" "$i 不是十进制" "$line"
    done
    # ppm 可以为 0：all.lua 报的是「离区间端点还差多少 ppm」，样本恰好命中端点时就是 0
    # （实测 `[minint + 0ppm, maxint - 2ppm]`）。把它当越界就是假红。
    ((10#$g2 <= 1000000 && 10#$g3 <= 1000000)) ||
      err_out "$name" "ppm 不在 [0,1000000]: $g2/$g3" "$line"
    REPL='integer random range in <N> calls: [minint + <P>ppm, maxint - <P>ppm]'
    ;;
  sort-timing)
    is_uint "$g2" || err_out "$name" "元素个数不是十进制: $g2" "$line"
    REPL="$g1 <N> $g3 elements in <T> msec."
    ;;
  sort-invert)
    is_uint "$g1" && is_uint "$g2" ||
      err_out "$name" "元素个数/比较次数不是十进制: $g1/$g2" "$line"
    REPL='Invert-sorting other <N> elements in <T> msec., with <M> comparisons'
    ;;
  sort-equal)
    is_uint "$g1" || err_out "$name" "元素个数不是十进制: $g1" "$line"
    REPL='sorting <N> equal elements in <T> msec.'
    ;;
  mainlua-kills)
    is_uint "$g1" || err_out "$name" "次数不是十进制: $g1" "$line"
    # 下限与 main.lua 自己的 assert(n >= 2) 同口径；越界即报错，不静默吞掉
    ((10#$g1 >= 2)) || err_out "$name" "成功次数 $g1 < 2（信号路径本身不对）" "$line"
    REPL='done (with <KILLS> kills)'
    ;;
  *)
    NORM_ERR="内部错误: 未知噪声类别 $name"
    return 2
    ;;
  esac
  # err_out 只负责记录原因；统一在这里把「任一校验不过」变成 rule_replace 的失败，
  # 否则分支里的 `|| err_out ...` 只影响那一条语句，越界值会被当成正常噪声吞掉。
  [[ -z $NORM_ERR ]] || return 2
  return 0
}

# ---- 归一化 ----------------------------------------------------------------
# normalize_file IN OUT [AUDIT]；AUDIT 非空时按 "类别<TAB>原始行" 追加命中记录。
normalize_file() {
  local infile=$1 outfile=$2 audit=${3:-}
  local -a A=() OUT=()
  local orig line core prefix name re hit m i
  mapfile -t A <"$infile" || { NORM_ERR="读取失败: $infile"; return 2; }
  for orig in "${A[@]}"; do
    orig=${orig%$'\r'}
    line=$orig
    # 进度点会**粘在下一个 print 出来的行首**：'.' 在输出缓冲里是连续的，换行只由
    # print() 产生，所以一串点总是落在下一条内容行的开头（实测：plain 变体里
    # `.    ---- total memory: 607.7K, max memory: 64.3M ----`）。直接拿整行去跑锚定 ^
    # 的规则会漏掉这一类，于是「同一棵 gcc 树跑两遍」都会报 SELF-NOISE-DIFF。
    # 做法：先把行首的点/加号串剥下来，只对剩下的部分跑规则，最后再把点剔掉。
    prefix=""
    core=$line
    if [[ $line =~ ^([.+]*)(.*)$ ]]; then
      prefix=${BASH_REMATCH[1]}
      core=${BASH_REMATCH[2]}
    fi
    for i in "${!RULE_NAMES[@]}"; do
      name=${RULE_NAMES[i]}
      re=${RULE_RES[i]}
      if [[ $core =~ $re ]]; then
        hit=${BASH_REMATCH[0]}
        if ! rule_replace "$name" "$orig"; then
          printf 'NORM-ERROR: %s\n' "$NORM_ERR" >&2
          return 2
        fi
        [ -n "$audit" ] && printf '%s\t%s\n' "$name" "$orig" >>"$audit"
        m=$REPL
        core=${core/"$hit"/"$m"}
      fi
    done
    line=${prefix}${core}
    line=${line//./}
    [[ -n ${line//[[:space:]]/} ]] || continue
    OUT+=("$line")
  done
  if ((${#OUT[@]} > 0)); then
    printf '%s\n' "${OUT[@]}" >"$outfile" || return 2
  else
    : >"$outfile" || return 2
  fi
  NORM_LINES=${#OUT[@]}
  return 0
}

# ---- 对拍 ------------------------------------------------------------------
# compare_logs REF CAND DIFFOUT [AUDIT]；0=相同 1=有差异 2=出错
compare_logs() {
  local ref=$1 cand=$2 dif=$3 audit=${4:-}
  local a b rc1 rc2
  a=$(mktemp "${TMPDIR:-/tmp}/lua_norm_a.XXXXXX") || return 2
  b=$(mktemp "${TMPDIR:-/tmp}/lua_norm_b.XXXXXX") || { rm -f "$a"; return 2; }
  normalize_file "$ref" "$a" "$audit"
  rc1=$?
  NORM_LINES_REF=$NORM_LINES
  normalize_file "$cand" "$b" "$audit"
  rc2=$?
  RC_CAND_LINES=$NORM_LINES
  if ((rc1 != 0 || rc2 != 0)); then
    rm -f "$a" "$b"
    return 2
  fi
  if cmp -s "$a" "$b"; then
    DIFFLINES=0
    rm -f "$a" "$b"
    return 0
  fi
  diff -U1 --label ref --label cand "$a" "$b" >"$dif"
  DIFFLINES=$(grep -cE '^[+-]' "$dif" 2>/dev/null)
  DIFFLINES=$((DIFFLINES - $(grep -cE '^(\+\+\+|---)' "$dif" 2>/dev/null || echo 0)))
  rm -f "$a" "$b"
  return 1
}

print_audit() { # $1=auditfile
  local af=$1 name n
  [[ -s $af ]] || return 0
  echo "归一化类别命中:"
  for name in "${RULE_NAMES[@]}"; do
    n=$(grep -c "^$name	" "$af" 2>/dev/null || true)
    ((n > 0)) || continue
    printf '  %-22s %5d 处\n' "$name" "$n"
    grep "^$name	" "$af" | head -2 | cut -f2- |
      sed 's/^[[:space:]]*//' | cut -c1-100 | sed 's/^/      e.g. /'
  done
  echo
}

# ---- 自测 ------------------------------------------------------------------
# 每条: expect|说明|ref|cand   （expect=same 归一化后必须相同；diff 必须不同）
# 文本里的 \n 由 printf %b 展开成真实换行。
SELFTEST_CASES=(
  "same|随机种子|random seeds: 1, -2|random seeds: 99, 5"
  "same|随机表长度种子|testing length for some random tables (seeds 0X8d913d89:8dc1d7a5e8b7567)|testing length for some random tables (seeds 0Xfba89a9d:42249dbc3102c86d)"
  "same|短路边号|testing short-circuit optimizations (0)|testing short-circuit optimizations (1)"
  "same|单文件耗时|time: 0.226494 (+0.226382)|time: 0.578111 (+0.57803)"
  "same|总耗时|total time: 3.93s (wall time: 6s)|total time: 11.08s (wall time: 13s)"
  "same|时间戳|test done on 26/10/2026, at 15:25:34|test done on 26/10/2026, at 16:17:09"
  "same|progname|progname: /tmp/Lg-O2/lua|progname: /tmp/lt-2/lua"
  "same|临时程序文件|(temporary program file used in these tests: /tmp/lua_H1h1P6)|(temporary program file used in these tests: /tmp/lua_jgyWkU)"
  "same|pid|(if test fails now, it may leave a Lua script running in background, pid 223113)|(if test fails now, it may leave a Lua script running in background, pid 318513)"
  "same|解释器路径前缀|/tmp/lsd/build/ref-ltests/src/lua: main.lua:67: exit|/tmp/lsd/build/dcc-O3-ltests/src/lua: main.lua:67: exit"
  "same|GC 计数（行尾带空格）|\t(strings:  205839, tables: 206402, functions: 1156, |\t(strings:  205988, tables: 206700, functions: 1305, "
  "same|GC 计数（第二行）|\tudata: 8, threads: 1)|\tudata: 9, threads: 3)"
  "same|GC 内存统计|    ---- total memory: 36.9M (35780K), max use: 85.6M,  blocks: 687517|    ---- total memory: 37.7M (36497K), max use: 88.5M,  blocks: 691546"
  "same|GC 内存统计（非 ltests 变体）|    ---- total memory: 2732K, max memory: 14M ----|    ---- total memory: 3678K, max memory: 14M ----"
  "same|随机浮点区间|float random range in 42400 calls: [0.000023, 0.999938]|float random range in 31800 calls: [0.000009, 0.999997]"
  "same|随机整数区间|integer random range in 44800 calls: [minint + 26ppm, maxint - 32ppm]|integer random range in 44800 calls: [minint + 17ppm, maxint - 36ppm]"
  "same|随机整数区间（样本命中端点，ppm=0）|integer random range in 38400 calls: [minint + 0ppm, maxint - 2ppm]|integer random range in 51200 calls: [minint + 53ppm, maxint - 24ppm]"
  "same|排序耗时|sorting 50000 random elements in 18.21 msec.|sorting 50000 random elements in 61.13 msec."
  "same|反向排序比较次数|Invert-sorting other 50000 elements in 48.97 msec., with 832290 comparisons|Invert-sorting other 50000 elements in 275.89 msec., with 828198 comparisons"
  "same|等值排序|sorting 50000 equal elements in 30.56 msec.|sorting 50000 equal elements in 189.72 msec."
  "same|SIGINT 用例的成功次数（竞态，main.lua 只断言 >=2）|done (with 2 kills)|done (with 3 kills)"
  "diff|真差异：SIGINT 用例文本被改坏|done (with 2 kills)|done (with 2 kill)"
  "same|重排序耗时|re-sorting 4000 sorted elements in 1.20 msec.|re-sorting 4000 sorted elements in 9.80 msec."
  "same|进度点位置|..testing failed write\ntesting large files (> BUFSIZ)|testing failed write..\ntesting large files (> BUFSIZ)"
  "same|行首粘进度点的内存行|.    ---- total memory: 607.7K, max memory: 64.3M ----|    ---- total memory: 607.6K, max memory: 64.3M ----"
  "same|行首粘进度点的耗时行|..time: 0.226494 (+0.226382)|time: 0.578111 (+0.57803)"
  "same|纯点行|.....\nOK|.\n....\nOK"
  # ---- 以下必须判为「不同」：归一化规则写宽了就会在这里暴露 ----
  "diff|真差异：测试名|testing gc|testing gcX"
  "diff|真差异：缺一行|board = 314159|"
  "diff|真差异：数值|stack: 100 200|stack: 100 201"
  "diff|真差异：类型尺寸|\tshort 2, int 4, long 8, size_t 8, float 4, double 8,|\tshort 2, int 4, long 8, size_t 8, float 4, double 4,"
  "diff|真差异：ABI 对齐|\talignment: 8|\talignment: 4"
  "diff|真差异：最小内存|minimum memory for state creation: 4655 bytes|minimum memory for state creation: 4672 bytes"
  "diff|真差异：Lua 版本|Lua 5.5.1  Copyright (C) 1994-2026 Lua.org, PUC-Rio|Lua 5.5.0  Copyright (C) 1994-2026 Lua.org, PUC-Rio"
  "diff|真差异：警告文本|Lua warning: #This is a single warning|Lua warning: #This is a single warnin"
  "diff|真差异：进度点串里的字母被吃掉|..testing files\ntestingAB|testing files\ntestingAC"
)

# 取值越界必须被拒（退出码 2），否则归一化器自己就成了假绿源。
# 注意：这里的用例必须「成形但超定义域」——模式本身已经限定了形状（如 pid 后只能接
# 数字），不像该形状的行不会被任何规则命中，它作为语义差异留在 diff 里，不属于
# 「越界拒绝」这一类判据。
SELFTEST_BAD_VALUES=(
  "短路边号越界|testing short-circuit optimizations (2)"
  "随机浮点区间越界|float random range in 10 calls: [0.5, 1.5]"
  "随机浮点区间端点反序|float random range in 10 calls: [0.9, 0.1]"
  "日期越界|test done on 32/10/2026, at 15:25:34"
  "时间越界|test done on 26/10/2026, at 25:25:34"
  "耗时不是浮点|time: 1.2.3 (+0.5)"
  "pid 越界|(if test fails now, it may leave a Lua script running in background, pid 9999999999)"
  "整数区间 ppm 越界|integer random range in 44800 calls: [minint + 2000000ppm, maxint - 32ppm]"
  "SIGINT 次数低于上游断言|done (with 1 kills)"
)

selftest() {
  local bad=0 total=0 rec expect desc a b dif rc tmpf tmpg
  dif=$(mktemp "${TMPDIR:-/tmp}/lua_norm_selftest_diff.XXXXXX") || return 2
  for rec in "${SELFTEST_CASES[@]}"; do
    IFS='|' read -r expect desc a b <<<"$rec"
    total=$((total + 1))
    tmpf=$(mktemp)
    tmpg=$(mktemp)
    printf '%b\n' "$a" >"$tmpf"
    printf '%b\n' "$b" >"$tmpg"
    compare_logs "$tmpf" "$tmpg" "$dif"
    rc=$?
    rm -f "$tmpf" "$tmpg"
    if [[ $expect == same ]]; then
      if ((rc == 0)); then
        printf 'ok   %s: 归一化后相同\n' "$desc"
      else
        printf 'FAIL %s: 归一化后不同（期望相同）\n' "$desc"
        sed -n '1,6p' "$dif" | sed 's/^/      /'
        bad=$((bad + 1))
      fi
    else
      if ((rc == 1)); then
        printf 'ok   %s: 归一化后仍不同（真差异没被吞）\n' "$desc"
      else
        printf 'FAIL %s: 注入的真差异被归一化吞掉了（rc=%d）\n' "$desc" "$rc"
        bad=$((bad + 1))
      fi
    fi
  done
  for rec in "${SELFTEST_BAD_VALUES[@]}"; do
    IFS='|' read -r desc a <<<"$rec"
    total=$((total + 1))
    tmpf=$(mktemp)
    printf '%b\n' "$a" >"$tmpf"
    normalize_file "$tmpf" "${tmpf}.norm" >/dev/null 2>&1
    rc=$?
    rm -f "$tmpf" "${tmpf}.norm"
    if ((rc == 2)); then
      printf 'ok   %s: 越界被拒\n' "$desc"
    else
      printf 'FAIL %s: 越界未被拒（rc=%d）\n' "$desc" "$rc"
      bad=$((bad + 1))
    fi
  done
  rm -f "$dif"
  printf '\nselftest: %d 通过, %d 失败\n' "$((total - bad))" "$bad"
  return $((bad == 0 ? 0 : 1))
}

usage() {
  cat <<'EOF'
用法: lua_suite_norm.sh REF_LOG CAND_LOG [--audit] [--max-diff N]
      lua_suite_norm.sh --selftest
退出码: 0 = 归一化后相同; 1 = 有差异; 2 = 用法错误或噪声取值越界
EOF
}

main() {
  local ref="" cand="" audit=0 maxdiff=400
  local -a pos=()
  while (($#)); do
    case $1 in
    --selftest)
      selftest
      return $?
      ;;
    --audit) audit=1 ;;
    --max-diff)
      shift
      [[ ${1:-} =~ ^[0-9]+$ ]] || { usage >&2; return 2; }
      maxdiff=$1
      ;;
    -h | --help)
      usage
      return 0
      ;;
    --*)
      echo "未知选项: $1" >&2
      usage >&2
      return 2
      ;;
    *) pos+=("$1") ;;
    esac
    shift
  done
  if ((${#pos[@]} != 2)); then
    echo "需要 REF_LOG 与 CAND_LOG（或用 --selftest）" >&2
    usage >&2
    return 2
  fi
  ref=${pos[0]}
  cand=${pos[1]}
  [[ -f $ref && -f $cand ]] || {
    echo "IO-ERROR: 日志不存在: $ref / $cand" >&2
    return 2
  }

  local af="" dif rc
  dif=$(mktemp "${TMPDIR:-/tmp}/lua_norm_diff.XXXXXX") || return 2
  ((audit)) && af=$(mktemp "${TMPDIR:-/tmp}/lua_norm_audit.XXXXXX")
  compare_logs "$ref" "$cand" "$dif" "$af"
  rc=$?
  ((audit)) && print_audit "$af"
  case $rc in
  0)
    printf 'IDENTICAL: 归一化后完全一致（%d 行有效输出）\n' "$NORM_LINES_REF"
    rm -f "$dif" "$af"
    return 0
    ;;
  1)
    printf 'NORM-DIFF: 归一化后仍有 %d 行差异\n' "$DIFFLINES"
    head -n "$maxdiff" "$dif"
    rm -f "$dif" "$af"
    return 1
    ;;
  *)
    rm -f "$dif" "$af"
    return 2
    ;;
  esac
}

if [[ ${BASH_SOURCE[0]} == "${0}" ]]; then
  main "$@"
  exit $?
fi
