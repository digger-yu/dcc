#!/usr/bin/env bash
# Batch compile arch/x86/ C files using dcc, reusing command template from
# a previous kernel build.
# Usage: bash scripts/arch_x86_compile.sh [kernel_dir]
set -u
KERNEL_DIR="${1:-/home/di/linux-7.1}"
DCC="${DCC:-/home/di/dcc/target/release/dcc}"
OUT_LOG="${OUT_LOG:-/tmp/arch_x86_compile.log}"
JOBS="${JOBS:-$(nproc 2>/dev/null || echo 8)}"

cd "$KERNEL_DIR" || exit 1

# Find a command template from existing .cmd files
CMD_FILE=""
for f in arch/x86/kernel/.*.o.cmd; do
    if [ -f "$f" ]; then
        CMD_FILE="$f"
        break
    fi
done

if [ -z "$CMD_FILE" ]; then
    echo "No command template found in arch/x86/kernel/.*.o.cmd" >&2
    echo "Please build the kernel with gcc first to generate .cmd files" >&2
    exit 1
fi

echo "Using command template from: $CMD_FILE" >&2
CMD_TEMPLATE=$(sed 's/^savedcmd_[^ ]* := //' "$CMD_FILE" | head -1)

# Build common flags from the template (everything before -c)
# Remove KBUILD_MODFILE/BASENAME/MODNAME as they vary per file
# Remove -Wp,-MMD paths as they vary per file
COMMON_FLAGS=""
skip_next=false
for a in $CMD_TEMPLATE; do
    if $skip_next; then skip_next=false; continue; fi
    case "$a" in
        -DKBUILD_MODFILE=*|-DKBUILD_BASENAME=*|-DKBUILD_MODNAME=*|-D__KBUILD_MODNAME=*)
            continue ;;
        -Wp,-MMD,*|-Wp,-MT,*)
            continue ;;
        -c|-o)
            break ;;
        *)
            COMMON_FLAGS="$COMMON_FLAGS $a" ;;
    esac
done

# Add dcc-specific flags
COMMON_FLAGS="$COMMON_FLAGS -Wno-implicit-declaration -DCC_USING_FENTRY"

# Filter out flags dcc doesn't implement
filtered=""
for a in $COMMON_FLAGS; do
    case "$a" in
        -fms-anonymous-structs|\
        -fzero-init-padding-bits=*|\
        -fdiagnostics-show-context=*|\
        -fdiagnostics-show-inlining-chain|\
        -fmin-function-alignment=*|\
        -fharden-sls=*|\
        -fzero-call-used-regs=*|\
        -fconserve-stack|\
        -fstrict-flex-arrays=*|\
        -fno-builtin-wcslen|\
        -mstack-protector-guard-reg=*|\
        -mstack-protector-guard-symbol=*|\
        -mharden-sls=*|\
        -mno-sse*|-mno-mmx|-mno-3dnow*|-mno-avx*|-mno-sse4*)
            continue ;;
        *)
            filtered="$filtered $a" ;;
    esac
done
COMMON_FLAGS="$filtered"

: > "$OUT_LOG"
: > "${OUT_LOG%.log}_errors.log"

compile_one() {
    local src="$1"
    local out="${src%.c}.o"
    local base=$(basename "$src" .c)
    local dir=$(dirname "$src")
    local modfile="$src"; modfile="${modfile%.c}"

    # Per-file KBUILD vars
    local per_file="-DKBUILD_MODFILE='\"$modfile\"' -DKBUILD_BASENAME='\"$base\"' -DKBUILD_MODNAME='\"$base\"' -D__KBUILD_MODNAME=$base"

    # Per-file dep file
    local dep="-Wp,-MMD,${dir}/.${base}.o.d -Wp,-MT,${out}"

    local cmd="$DCC $COMMON_FLAGS $per_file $dep -c -o $out $src"

    if ! eval "$cmd" >> "$OUT_LOG" 2>&1; then
        echo "FAIL $src" >> "$OUT_LOG"
        # Extract just errors for analysis
        eval "$cmd" 2>&1 | grep '^error' >> "${OUT_LOG%.log}_errors.log" 2>/dev/null
        return 1
    fi
    return 0
}

export -f compile_one
export DCC COMMON_FLAGS OUT_LOG

# Collect files: use make -n to get the actual build set, or fallback to find
if [ "${USE_MAKE:-0}" = "1" ]; then
    touch arch/x86/kernel/*.c arch/x86/mm/*.c arch/x86/lib/*.c 2>/dev/null
    make -n arch/x86/ 2>/dev/null | grep -oE 'arch/x86/[a-zA-Z0-9_./-]+\.c' \
        | sort -u \
        | grep -vE '\.(o|s)\.c$' \
        | grep -vE '/\.' \
        | grep -vE '(^|/)asm-offsets\.c$|(^|/)bounds\.c$' \
        > /tmp/arch_x86_files.txt
else
    find arch/x86 -name '*.c' \
        -not -path '*/boot/*' \
        -not -path '*/purgatory/*' \
        -not -path '*/realmode/*' \
        -not -path '*/um/*' \
        -not -name 'asm-offsets*' \
        -not -name 'bounds.c' \
        | sort > /tmp/arch_x86_files.txt
fi

total=$(wc -l < /tmp/arch_x86_files.txt)
echo "== compiling $total arch/x86/ files with $JOBS jobs =="
echo "== using flags: $COMMON_FLAGS" | head -c 200
echo "..."

pass=0
fail=0
while IFS= read -r f; do
    if compile_one "$f"; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
    fi
done < /tmp/arch_x86_files.txt

echo ""
echo "== results =="
echo "PASS: $pass / $total"
echo "FAIL: $fail / $total"
echo ""
echo "== error summary =="
if [ -f "${OUT_LOG%.log}_errors.log" ]; then
    grep '^error' "${OUT_LOG%.log}_errors.log" | sort | uniq -c | sort -rn | head -20
fi
