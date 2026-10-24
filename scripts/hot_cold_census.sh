#!/bin/bash
# ===========================================================================
# hot_cold_census.sh — computed-goto 函数的「三账 + 落脚」普查（方案 §1/§5 的判据来源）
#
# 为什么不用 fn_opcode_heat.sh 出这三账：后者的区间归属是「最后一个 ≤addr 的标签」，
# 末段把 `≥max(label)` 的冷尾一起吸进最后一个区间，于是**永远看不出 OUT 箱**。而 gcc 与
# dcc 差距的第一主轴正是「冷尾搬没搬出热区」，所以这里把三账拆开且必须同列：
#   entry = addr <  min(label)                        函数入口/序言
#   hot   = min(label) ≤ addr < max(label)            被跳表指向的臂区间
#   out   = addr ≥  max(label)                        冷尾/外置块
# 三账之和 = full。任何机制只让 hot 降而 out 不升，就是「搬运」不是收益（方案 §7 假绿）。
#
# 落脚口径（P0/P1 的判据，按区重置——与 blockopt 的真实语义一致）：
#   pairs     同一地址「先存后取」且中间没再存过该地址的对数
#   pure      纯落脚：存进去的源寄存器在取出前没被改写 ⇒ remat/直传可消
#   reused    源寄存器被覆写 ⇒ 槽必须真分配，remat 无效
#   as_base   载入的值被当内存基址用（地址中转）
#   disp8/disp32  帧访存按位移宽度分列（编码器真判据 -128..=127，见 gas.rs:92）
#
# 计数口径（防续行）：一律 `objdump -d -M intel --no-show-raw-insn`，一行一条指令。
# 若改用保留 raw 字节的管道，助记符在第 3 字段且必须过滤续行，两者不可混用。
#
# 用法:
#   bash scripts/hot_cold_census.sh <binary> <sym> [表符号正则]
#   例: bash scripts/hot_cold_census.sh /root/lua-5.5.1/src/lvm.o luaV_execute
#       bash scripts/hot_cold_census.sh /tmp/bench_lua_sqlite.txt.artifacts/lua_gcc_O2 luaV_execute
# 环境:
#   NAMES=<文件>  每行一个区间名（与表项同序）；缺省只报序号
#   TOP=<n>       臂明细列出的条数（默认 12，按条数降序）
#   TMP=<前缀>    临时文件前缀（默认 /tmp/hcc）
# 臂入口来源：链接后产物读跳表数值；可重定位对象里跳表项是重定位（值为 0），
#   自动退回 `readelf -r` 取 `.text+0x…` 加数（labels_src=reloc）。
# ===========================================================================
set -o pipefail
set -u

BIN=${1:?用法: hot_cold_census.sh <binary> <sym> [表符号正则]}
SYM=${2:?需要函数符号名}
PAT=${3:-disptab|jumpTable}
TOP=${TOP:-12}
TMP=${TMP:-/tmp/hcc}

# mawk 既没有 strtonum 也没有 and()/xor()，十六进制与小端拼装一律自己写；
# 这段函数库必须拼在每个用到 h2d 的 awk 程序前面。
AWKF='function h2d(s,  i,c,v){v=0;s=tolower(s);sub(/^0x/,"",s);if(s=="")return 0;for(i=1;i<=length(s);i++){c=substr(s,i,1);v=v*16+index("0123456789abcdef",c)-1}return v}
function bswap4(w,  b0,b1,b2,b3){b0=w%256;b1=int(w/256)%256;b2=int(w/65536)%256;b3=int(w/16777216)%256;return b0*16777216+b1*65536+b2*256+b3}
function nrm(r,  k){
  if (r=="" ) return r
  if (r=="rip") return "rip"
  # 64 位本名直接返回
  if (r=="rax"||r=="rbx"||r=="rcx"||r=="rdx"||r=="rsi"||r=="rdi"||r=="rbp"||r=="rsp") return r
  if (r ~ /^r[0-9]+$/) return r
  k=r
  sub(/[dxsb]$/, "", k)          # r12d -> r12 / eax 走不到这里
  if (k ~ /^r[0-9]+$/ && k!=r) return k
  # 8/16/32 位的老命名：al ah ax eax ... 一律归到 64 位本名
  if (r ~ /^(al|ah|ax|eax)$/) return "rax"
  if (r ~ /^(bl|bh|bx|ebx)$/) return "rbx"
  if (r ~ /^(cl|ch|cx|ecx)$/) return "rcx"
  if (r ~ /^(dl|dh|dx|edx)$/) return "rdx"
  if (r ~ /^(sil|si|esi)$/)   return "rsi"
  if (r ~ /^(dil|di|edi)$/)   return "rdi"
  if (r ~ /^(bpl|bp|ebp)$/)   return "rbp"
  if (r ~ /^(spl|sp|esp)$/)   return "rsp"
  if (r ~ /^ip$/)             return "rip"
  return r
}'

fs=$(readelf -sW "$BIN" | awk -v s="$SYM" '$8==s && $3>0 {print $2, $3; exit}')
[ -n "$fs" ] || { echo "hot_cold_census: $BIN 里没有带大小的符号 $SYM" >&2; exit 1; }
START=$(awk "$AWKF"'BEGIN{print h2d(ARGV[1])}' "${fs%% *}")
SIZE=${fs##* }
END=$((START + SIZE))

# ---------- 臂入口标签 ----------
labels_src=table
info=$(readelf -sW "$BIN" | awk -v p="$PAT" '$8 ~ p && $3>0 {print $2, $3; exit}')
: > "$TMP.labels"
if [ -n "$info" ]; then
  ta=$((16#${info%% *})); ts=${info##* }; te=$((ta + ts))
  objdump -s --start-address=$ta --stop-address=$te "$BIN" 2>/dev/null |
    awk 'NF>=2 && $1 ~ /^[0-9a-f]+$/ {for(i=2;i<=NF;i++) if($i ~ /^[0-9a-f]+$/) print $i}' > "$TMP.words"
  awk "$AWKF"'{w[NR]=h2d($1)} END{for(i=1;i+1<=NR;i+=2){v=bswap4(w[i])+bswap4(w[i+1])*4294967296;
       if(v>='"$START"' && v<'"$END"') print v}}' "$TMP.words" | sort -n | uniq > "$TMP.labels"
  rm -f "$TMP.words"
fi
if [ ! -s "$TMP.labels" ]; then
  # 可重定位对象：表项是重定位，值读出来是 0。臂入口只能从 `R_X86_64_64 .text + <十六进制加数>` 拿。
  # 加数不带 0x 前缀且是十六进制（与 objdump -dr 的 `.text+0x5d6b` 同值）；LC_ALL=C 防中文列名抖字段。
  labels_src=reloc
  LC_ALL=C readelf -rW "$BIN" 2>/dev/null |
    awk "$AWKF"'$3=="R_X86_64_64" && $5==".text" && $6=="+" {
        v=h2d($7); if (v>='"$START"' && v<'"$END"') print v }' |
    sort -n | uniq > "$TMP.labels"
fi
[ -s "$TMP.labels" ] || { echo "hot_cold_census: 取不到臂入口标签（$PAT 没命中或表项不可解析）" >&2; exit 1; }

objdump -d -M intel --no-show-raw-insn --start-address=$START --stop-address=$END "$BIN" > "$TMP.dis"

# ---------- 三账 + 帧/落脚（单遍） ----------
awk "$AWKF"'
  NR==FNR { lab[++n]=$1+0; next }
  /^[[:space:]]+[0-9a-f]+:/ {
    a=$1; gsub(":","",a); addr=h2d(a)
    if (addr < '"$START"' || addr >= '"$END"') next
    total++
    txt=""; for(i=2;i<=NF;i++) txt=txt (i>2?" ":"") $i
    mn=txt; sub(/ .*/,"",mn)

    # ---- 三账；同时按区（标签）边界重置落脚状态，与 blockopt 语义一致 ----
    if (addr < lab[1])            entry++
    else if (addr >= lab[n])      out++
    else {
      hot++
      while(k<n && lab[k+1]<=addr) k++
      if (cur != lab[k]) { cur=lab[k]; delete sreg; delete spc; delete wr; delete from_mem }
      reg[lab[k]]++
    }

    # ---- 操作数切分：mnemonic 之后按逗号切，允许 "QWORD PTR [x]" 这类带空格的形态 ----
    rest=substr(txt, length(mn)+1); sub(/^ +/,"",rest)
    gsub(/  +/," ",rest)
    nc=split(rest, raw, ",")
    for(q=1;q<=nc;q++){ sub(/^ +/,"",raw[q]); sub(/ +$/,"",raw[q]) }

    # ---- 寄存器写时刻（cmp/test/call/ret/push 不写目的号；lea 会写）----
    touched=0
    if (nc>=1 && raw[1] !~ /\[/ && raw[1] !~ /^0x/ && raw[1] !~ /^PTR/ &&
        mn !~ /^(cmp|test|call|ret|push)$/) { wr[nrm(raw[1])]=addr; touched=1 }

    # ---- 内存操作数解析 ----
    dm=""; sm=""; dmem=0; smem=0
    if (nc==2) {
      if (raw[1] ~ /\[/) { dmem=1; dm=raw[1] } else if (raw[2] ~ /\[/) { smem=1; sm=raw[2] }
    }

    # ---- 落脚配对：同一地址「先存后取」且中间没再存过该地址 ----
    #     pure   = 存进去的源寄存器在取出前没被改写 ⇒ remat/直传可消
    #     reused = 源寄存器被覆写 ⇒ 槽必须真分配
    if (mn ~ /^(mov|movzx|movsx|cmov)/ || (mn ~ /^(add|sub|and|or|xor)$/ && dmem)) {
      if (dmem) {
        dk=parsemem(dm)
        if (dk!="" && nc==2 && raw[2] !~ /\[/) { sreg[dk]=nrm(raw[2]); spc[dk]=addr }
      } else if (smem && nc==2) {
        sk=parsemem(sm)
        if (sk!="" && (sk in sreg)) {
          pairs++
          if (wr[sreg[sk]] > spc[sk] && wr[sreg[sk]] != addr) reused++
          else { pure++; if (nrm(raw[1])==sreg[sk]) same++ }
          delete sreg[sk]; delete spc[sk]
        }
      }
    }

    # ---- 「值来自内存」追踪：载入、lea、以及它们的复制都会染上地址性 ----
    #      as_base 的语义就是「刚从内存取出来的值又被当基址用」，即地址中转。
    if (mn=="lea" && nc==2) from_mem[nrm(raw[1])]=1
    else if (mn ~ /^(mov|movzx|movsx|cmov)/ && nc==2) {
      if (smem) from_mem[nrm(raw[1])]=1
      else if (raw[2] !~ /\[/ && raw[2] !~ /^0x/) {
        s2r=nrm(raw[2]); from_mem[nrm(raw[1])] = (s2r in from_mem) ? from_mem[s2r] : 0
      } else from_mem[nrm(raw[1])]=0
    }
    else if (touched) from_mem[nrm(raw[1])]=0

    # ---- 任何内存操作数：位移宽度普查 + 「取出即作基址」判定 ----
    for(q=1;q<=nc;q++) if (raw[q] ~ /\[/) {
      mk=parsemem(raw[q]); split(mk, e, "|")
      b=e[1]; dp=e[4]+0
      if (b=="r13" || b=="rsp" || b=="rbp") {
        fr++
        if (b=="r13") r13++; else if (b=="rsp") frsp++; else frbp++
        if (dp>=-128 && dp<=127) d8++; else d32++
        refs[b"|"dp]++
      }
      # 基址是「刚从内存取出来的值」⇒ 计一次地址中转，并摘掉地址性免重复计
      if ((b in from_mem) && from_mem[b]==1) { as_base++; from_mem[b]=0 }
      if (mn=="lea" && q==2 && (b=="r13" || b=="rsp")) { lea++; if (dp<-128 || dp>127) lea32++; else lea8++ }
    }
    next
  }
  # 内存操作数 -> "base|index|scale|disp"。
  # objdump 的 Intel 语法把基址与位移写成 `[r13-0x650]`（无逗号），只有带变址才有逗号：
  # `[rbx+rcx*8]`、`[rip+0x123]`。所以先把 '-' 归一成 '+-' 再按 '+' 切，每段要么是寄存器、
  # 要么是 `reg*scale`、要么是（可带负号的）数值。
  function parsemem(s,   c1,c2,body,np,toks,i,t,b,ix,sc,dp) {
    c1=index(s,"["); c2=index(s,"]")
    if (c1==0 || c2==0) return ""
    body=tolower(substr(s,c1+1,c2-c1-1))
    gsub(/[ \t]/,"",body)
    gsub(/-/,"+-",body)
    np=split(body,toks,"+")
    b=""; ix=""; sc=0; dp=0
    for(i=1;i<=np;i++) {
      t=toks[i]; if (t=="") continue
      if (t ~ /\*/) { split(t,p2,"*"); ix=nrm(p2[1]); sc=p2[2]+0 }
      else if (t ~ /^-?(0x)?[0-9a-f]+$/ && t !~ /^[a-z]/) {
        if (substr(t,1,1)=="-") dp -= h2d(substr(t,2)); else dp += h2d(t)
      }
      else if (b=="") b=nrm(t)
      else ix=nrm(t)
    }
    return b"|"ix"|"sc"|"dp
  }
  END {
    printf "full=%d entry=%d hot=%d out=%d\n", total+0, entry+0, hot+0, out+0
    printf "frame_refs=%d r13=%d rsp=%d rbp=%d disp8=%d disp32=%d lea_frame=%d(d8=%d/d32=%d)\n",
           fr+0, r13+0, frsp+0, frbp+0, d8+0, d32+0, lea+0, lea8+0, lea32+0
    printf "landing_pairs=%d pure=%d reused=%d same_reg_kept=%d as_base=%d\n",
           pairs+0, pure+0, reused+0, same+0, as_base+0
    mx=0; mn2=-1; sumr=0; cnt=0
    for(i=1;i<=n;i++){ c=reg[lab[i]]+0; sumr+=c; cnt++; if(c>mx)mx=c; if(mn2<0||c<mn2)mn2=c }
    al=0; for(i=1;i<=n;i++) if (lab[i] % 16 == 0) al++
    printf "arms=%d arm_mean=%.1f arm_min=%d arm_max=%d arm_sum=%d arm_align16=%d labels_src=%s\n",
           n, (cnt?sumr/cnt:0), mn2, mx, sumr, al, "'"$labels_src"'"
    # 最忙的 8 个帧槽（引用次数降序）
    nn=0; for(k in refs){ v[++nn]=refs[k]" "k }
    nsort(nn)
    printf "top_slots:"
    for(i=1;i<=8 && i<=nn;i++) printf " %s", v[i]
    printf "\n"
  }
  function nsort(m,   i,j,t){ for(i=2;i<=m;i++){ t=v[i]; j=i-1; while(j>=1 && v[j]+0<t+0){ v[j+1]=v[j]; j-- } v[j+1]=t } }
' "$TMP.labels" "$TMP.dis"

# ---------- 臂明细（条数降序） ----------
awk "$AWKF"'
  NR==FNR { lab[++n]=$1+0; next }
  /^[[:space:]]+[0-9a-f]+:/ {
    a=$1; gsub(":","",a); addr=h2d(a)
    if (addr < '"$START"' || addr >= '"$END"') next
    if (addr < lab[1] || addr >= lab[n]) next
    while(k<n && lab[k+1]<=addr) k++
    reg[lab[k]]++
  }
  END{ for(i=1;i<=n;i++) print i, lab[i], reg[lab[i]]+0 }' "$TMP.labels" "$TMP.dis" |
  sort -k3,3nr | head -"$TOP" |
  awk -v namef="${NAMES:-}" '
    BEGIN{ while(namef!="" && (getline l < namef)>0) nm[++m]=l }
    { printf "arm#%s %-16s addr=0x%x insns=%-5d\n", $1, (nm[$1]!=""?nm[$1]:"(unnamed)"), $2, $3 }'
