#!/bin/bash
# 用 dcc 试编 GCC libiberty（纯 C 子集），收集报错
set -e
DCC="${DCC:-/home/ds/dcc/target/release/dcc}"
GCCSRC="${GCCSRC:-/home/ds/gcc-src}"
OUT="${OUT:-/tmp/dcc-gcc-build}"
rm -rf "$OUT" && mkdir -p "$OUT"

if [ ! -f "$GCCSRC/libiberty/config.h" ]; then
  cat > "$GCCSRC/libiberty/config.h" << 'EOF'
#ifndef LIBIBERTY_CONFIG_H
#define LIBIBERTY_CONFIG_H
#define HAVE_STDLIB_H 1
#define HAVE_STRING_H 1
#define HAVE_STRINGS_H 1
#define HAVE_UNISTD_H 1
#define HAVE_FCNTL_H 1
#define HAVE_SYS_FILE_H 1
#define HAVE_SYS_PARAM_H 1
#define HAVE_SYS_STAT_H 1
#define HAVE_SYS_TIME_H 1
#define HAVE_SYS_RESOURCE_H 1
#define HAVE_SYS_WAIT_H 1
#define HAVE_SYS_MMAN_H 1
#define HAVE_TIME_H 1
#define HAVE_LIMITS_H 1
#define HAVE_STDINT_H 1
#define HAVE_INTTYPES_H 1
#define HAVE_MEMORY_H 1
#define HAVE_ALLOCA_H 1
#define HAVE_ALLOCA 1
#define HAVE_ATEXIT 1
#define HAVE_GETCWD 1
#define HAVE_GETPAGESIZE 1
#define HAVE_GETTIMEOFDAY 1
#define HAVE_ASPRINTF 1
#define HAVE_VASPRINTF 1
#define HAVE_SNPRINTF 1
#define HAVE_VSNPRINTF 1
#define HAVE_STRDUP 1
#define HAVE_STRNLEN 1
#define HAVE_STRNDUP 1
#define HAVE_STRERROR 1
#define HAVE_STRSIGNAL 1
#define HAVE_STPCPY 1
#define HAVE_STPNCPY 1
#define HAVE_MEMCPY 1
#define HAVE_MEMMOVE 1
#define HAVE_MEMSET 1
#define HAVE_MEMCHR 1
#define HAVE_MEMRCHR 1
#define HAVE_MEMPCPY 1
#define HAVE_MALLOC 1
#define HAVE_REALLOC 1
#define HAVE_CALLOC 1
#define HAVE_MMAP 1
#define HAVE_POPEN 1
#define HAVE_REALPATH 1
#define HAVE_SBRK 1
#define HAVE_SETENV 1
#define HAVE_UNSETENV 1
#define HAVE_WAITPID 1
#define HAVE_SYSCONF 1
#define HAVE_DECL_ASPRINTF 1
#define HAVE_DECL_BASENAME 1
#define HAVE_DECL_CALLOC 1
#define HAVE_DECL_FFS 1
#define HAVE_DECL_GETENV 1
#define HAVE_DECL_GETOPT 1
#define HAVE_DECL_MALLOC 1
#define HAVE_DECL_REALLOC 1
#define HAVE_DECL_SBRK 1
#define HAVE_DECL_SNPRINTF 1
#define HAVE_DECL_VASPRINTF 1
#define HAVE_DECL_VSNPRINTF 1
#define HAVE_UINTPTR_T 1
#define SIZEOF_INT 4
#define SIZEOF_LONG 8
#define SIZEOF_SIZE_T 8
#define SIZEOF_LONG_LONG 8
#define STACK_DIRECTION -1
#define UNSIGNED_64BIT_TYPE unsigned long
#endif
EOF
fi

OK=0
FAIL=0
FAILLIST=""
for src in "$GCCSRC"/libiberty/*.c; do
  f=$(basename "$src" .c)
  case "$f" in soft-fp*|test*) continue ;; esac
  if "$DCC" -c -I"$GCCSRC/include" -I"$GCCSRC/libiberty" \
      -o "$OUT/${f}.o" "$src" 2>"$OUT/${f}.err"; then
    OK=$((OK + 1))
    echo "OK $f"
  else
    FAIL=$((FAIL + 1))
    FAILLIST="$FAILLIST $f"
    echo "FAIL $f:"
    sed 's/\x1b\[[0-9;]*m//g' "$OUT/${f}.err" | head -10
    echo
  fi
done
echo "==== SUMMARY ok=$OK fail=$FAIL ===="
echo "Failed:$FAILLIST"
