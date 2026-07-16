#!/bin/bash
set -euo pipefail
export PATH="/usr/bin:/bin:$HOME/.cargo/bin:$PATH"
DCC=/home/ds/dcc/target/release/dcc
GCCSRC=/home/ds/gcc-src

# 在 matcher 函数附近查找 #define d
grep -n '#define d\b\|#define d \|#define d$' "$GCCSRC/libiberty/regex.c" || echo "no #define d in regex.c"

# 预处理到 matcher 行之前，dump 宏 d（通过故意触发）
cat > /tmp/check_d_macro.c << 'EOF'
#define RE_DOT_NEWLINE 0
#define RE_DOT_NOT_NULL 0
typedef unsigned char UCHAR_T;
typedef const unsigned char *CHAR_T;
typedef unsigned char *RE_TRANSLATE_TYPE;
struct re_pattern_buffer { int syntax; RE_TRANSLATE_TYPE translate; };
typedef struct re_pattern_buffer *re_pattern_buffer;

# define TRANSLATE(d) \
  (translate ? (char) translate[(unsigned char) (d)] : (char) (d))

#define PREFETCH() while (d == dend) { if (dend == end_match_2) goto fail; d = string2; dend = end_match_2; }

/* 模拟 matcher 内 register const char *d 存在时，若 d 被误定义为宏会怎样 */
#define d (*p)
void test(re_pattern_buffer bufp, register const char *d, register const char *dend,
          RE_TRANSLATE_TYPE translate, const char *p) {
  PREFETCH ();
  if (TRANSLATE (*d) == '\n') return;
}
EOF

echo "==== with #define d (*p) ===="
$DCC -E /tmp/check_d_macro.c 2>&1 | grep -E 'translate|\*\*d|\*d|while \(d'

cat > /tmp/check_d_macro2.c << 'EOF'
# define TRANSLATE(d) \
  (translate ? (char) translate[(unsigned char) (d)] : (char) (d))
CHAR_T *translate;
typedef unsigned char CHAR_T;
void f(const CHAR_T *d) {
  if (TRANSLATE (*d) == '\n') return;
}
EOF

echo "==== without d macro (baseline) ===="
$DCC -E /tmp/check_d_macro2.c 2>&1 | grep translate
