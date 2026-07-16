#!/bin/bash
set -euo pipefail
export PATH="/usr/bin:/bin:$HOME/.cargo/bin:$PATH"
DCC=/home/ds/dcc/target/release/dcc
GCCSRC=/home/ds/gcc-src

# 从 regex.c 提取 matcher 片段 + 宏定义
cat > /tmp/regex_snip.c << 'EOF'
#define RE_DOT_NEWLINE 0
#define RE_DOT_NOT_NULL 0
typedef unsigned char UCHAR_T;
typedef const unsigned char *CHAR_T;
typedef unsigned char *RE_TRANSLATE_TYPE;
struct re_pattern_buffer { int syntax; RE_TRANSLATE_TYPE translate; };
typedef struct re_pattern_buffer *re_pattern_buffer;

# define TRANSLATE(d) \
  (translate ? (char) translate[(unsigned char) (d)] : (char) (d))

#define PREFETCH()							\
  while (d == dend) { d = dend; }

void test(re_pattern_buffer bufp, register const char *d, register const char *dend,
          RE_TRANSLATE_TYPE translate) {
  case anychar:
    PREFETCH ();
    if ((!(bufp->syntax & RE_DOT_NEWLINE) && TRANSLATE (*d) == '\n')
        || (bufp->syntax & RE_DOT_NOT_NULL && TRANSLATE (*d) == '\000'))
      return;
}
EOF

echo "==== snip preprocess ===="
$DCC -E /tmp/regex_snip.c 2>&1 | tee /tmp/regex_snip.i
echo "==== **d count ===="
grep -c '\*\*d' /tmp/regex_snip.i || echo 0
echo "==== PREFETCH expanded? ===="
grep -c 'while (d == dend)' /tmp/regex_snip.i || echo 0
