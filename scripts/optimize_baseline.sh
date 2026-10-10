#!/bin/bash
# scripts/optimize_baseline.sh - Performance Baseline Collection
# Phase 4: Optimization and Tuning

set -e

echo "=== DCC Compiler Performance Baseline Collection ==="
echo "Date: $(date)"
echo ""

# Create results directory
RESULTS_DIR="/tmp/dcc_optimization_benchmarks_$(date +%Y%m%d_%H%M%S)"
mkdir -p "$RESULTS_DIR"

echo "Results will be stored in: $RESULTS_DIR"
echo ""

# Benchmark categories
# 路径已对齐到磁盘上真实存在的夹具/示例: 旧的 tests/fixtures/{basic,expressions,control,memory}/
# 布局早已不存在(门禁报 7 个 MISSING), 本脚本此前对全部 7 项都只走
# "file not found -> skip" 分支, 一个数据都没采到却仍打印 "Baseline Collection Complete"。
# 现在逐项按语义等价选型, 全部经 dcc 实验可编译(见下方 rc 判定)。
BENCHMARKS=(
    "tests/fixtures/smoke/arith.c:simple_add"
    "tests/fixtures/smoke/compound_assign.c:basic_subtract"
    "tests/fixtures/smoke/logic.c:binary_operations"
    "tests/fixtures/smoke/switch.c:if_else_branching"
    "tests/fixtures/smoke/control_flow.c:loop_iteration"
    "tests/fixtures/smoke/float_cast.c:type_casting"
    "tests/fixtures/smoke/global_array.c:array_indexing"
)

echo "Running compilation benchmarks..."
echo ""

for bench in "${BENCHMARKS[@]}"; do
    IFS=':' read -r source test_name <<< "$bench"
    
    if [ ! -f "$source" ]; then
        echo "⚠️  Skipping $test_name (file not found: $source)"
        continue
    fi
    
    echo "Testing: $test_name"
    echo "Source: $source"
    
    # Capture compilation time (monolith version)
    start_time=$(date +%s.%N)
    if dcc "$source" -m=x86_64 -o "$RESULTS_DIR/${test_name}.monolith.o" 2>/dev/null; then
        end_time=$(date +%s.%N)
        elapsed=$(echo "$end_time - $start_time" | bc)
        
        # Get binary size
        binary_size=$(stat --printf='%s' "$RESULTS_DIR/${test_name}.monolith.o")
        
        echo "  ✅ Monolith: ${elapsed}s, Size: ${binary_size} bytes"
        
        # Record in results file
        echo "${test_name},monolith,${elapsed},${binary_size}" >> "$RESULTS_DIR/metrics.csv"
    else
        echo "  ❌ Compilation failed!"
        echo "${test_name},monolith,FAIL,0" >> "$RESULTS_DIR/metrics.csv"
    fi
    
    echo ""
done

echo "=== Baseline Collection Complete ==="
echo "Metrics saved to: $RESULTS_DIR/metrics.csv"
echo ""

# Display summary
if [ -f "$RESULTS_DIR/metrics.csv" ]; then
    echo "Summary:"
    echo "--------"
    cat "$RESULTS_DIR/metrics.csv"
fi

echo ""
echo "Next steps:"
echo "1. Enable new-expressions feature flag"
echo "2. Re-run same benchmarks with shared library"  
echo "3. Compare metrics.csv against baseline"
echo "4. Target: <5% performance degradation acceptable"
