#!/usr/bin/env bash
# 批量编译指定的 C++ fixture 文件，收集每个的第一行 error 输出
set -u

DCC="/root/dcc/target/release/dcc"
FIXTURE_DIR="/root/dcc/tests/fixtures/cpp"

# 用户指定的 fixture 列表
FIXTURES=(
    coroutine_co_await
    using_type_alias
    cpp20_features
    cpp23_features
    iostream_basic
    iterator_traits
    vector_int
    vector_header
    auto_deduce
    decltype_auto
    explicit_ctor
    friend
    if_constexpr
    inheriting_ctor
    mutable
    noexcept
    weak_vtable
    chrono
    concept
    condition_variable
    expected
    filesystem
    flat_map
    multi_index
    ranges
    regex
    sfinae
    thread_mutex
    auto_deduction
    coroutine_runtime
    coroutine_state
    enum_semantics
    gcc_compat
    member_parser
    modules
    modules_odr
    noexcept_expr
    odr_conflict
    virtual_inheritance
    member_ptr_deref
    mutable_const
    template_specialization
    typeid_rtti
    member_ptr_dotstar
    template_arg
    semantic_run
)

echo "=== Fixture → 首个 error 行 ==="
echo

for base in "${FIXTURES[@]}"; do
    # 主尝试
    try_bases=("$base")
    # 如果不存在，尝试 _runtime 后缀映射
    if [ ! -f "${FIXTURE_DIR}/${base}.cpp" ]; then
        # 有没有 base_runtime.cpp 存在？
        if [ -f "${FIXTURE_DIR}/${base}_runtime.cpp" ]; then
            try_bases+=("${base}_runtime")
        fi
        # 有没有 p2_ 前缀的？
        for f in "${FIXTURE_DIR}"/p2_*.cpp; do
            [ -f "$f" ] || continue
            p2_name=$(basename "$f" .cpp)
            # 简单策略：如果 base 的子串匹配，试一下
            if [[ "$p2_name" == *"$base"* ]]; then
                try_bases+=("$p2_name")
            fi
        done
    fi

    chosen=""
    for candidate in "${try_bases[@]}"; do
        if [ -f "${FIXTURE_DIR}/${candidate}.cpp" ]; then
            chosen="$candidate"
            break
        fi
    done

    if [ -z "$chosen" ]; then
        echo "$base :: (文件不存在，未找到 _runtime 或 p2_* 替代) :: NO_SUCH_FILE"
        continue
    fi

    # 运行编译；捕获 stderr；找第一行包含 error 的
    out=$("$DCC" -c "${FIXTURE_DIR}/${chosen}.cpp" 2>&1)
    first_error=$(echo "$out" | grep -m1 -i 'error' || true)
    if [ -z "$first_error" ]; then
        # 也可能编译成功了
        echo "$base :: $chosen :: (无 error 输出，可能已编译成功)"
    else
        echo "$base :: $chosen :: $first_error"
    fi
done
