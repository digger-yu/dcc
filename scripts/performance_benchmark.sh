#!/bin/bash
# scripts/performance_benchmark.sh - Quick Performance Test
# Phase 4: Immediate optimization validation

set -e

echo "=== DCC Compiler Quick Performance Benchmark ==="
echo "Date: $(date)"
echo ""

# Use existing tests from GCC torture suite
TEST_DIR="tests/fixtures/c"
RESULTS_FILE="/tmp/dcc_quick_perf_$(date +%Y%m%d_%H%M%S).csv"

echo "Running quick benchmarks on $TEST_DIR..."
echo ""

# Simple benchmark loop
total_tests=0
passed_tests=0
failed_tests=0

for test in "$TEST_DIR"/*.c; do
    if [ ! -f "$test" ]; then
        continue
    fi
    
    filename=$(basename "$test")
    
    # Skip very large files for quick test
    if [ "$(wc -l < "$test")" -gt 1000 ]; then
        echo "⏭️  Skipping large file: $filename ($(wc -l < "$test") lines)"
        continue
    fi
    
    echo -n "Testing $filename... "
    
    start_time=$(date +%s.%N)
    
    if dcc "$test" -m=x86_64 -o "/tmp/out.o" 2>/dev/null; then
        end_time=$(date +%s.%N)
        elapsed=$(echo "$end_time - $start_time" | bc)
        
        binary_size=$(stat --printf='%s' "/tmp/out.o" 2>/dev/null || echo "0")
        
        echo "✅ ${elapsed}s (${binary_size}B)"
        
        echo "$filename,$elapsed,$binary_size" >> "$RESULTS_FILE"
        
        ((total_tests++)) || true
        
        # Track average for first few tests
        if [ $passed_tests -lt 5 ]; then
            running_sum=$(echo "$running_sum + $elapsed" | bc 2>/dev/null || echo "$elapsed")
            test_count=$(((test_count + 1)))
        fi
        ((passed_tests++)) || true
    else
        echo "❌ FAILED"
        echo "$filename,FAIL,0" >> "$RESULTS_FILE"
        ((failed_tests++)) || true
    fi
    
done

echo ""
echo "=== Benchmark Summary ==="
echo "Total tests: $total_tests"
echo "Passed: $passed_tests"  
echo "Failed: $failed_tests"

if [ $test_count -gt 0 ] && [ -n "$running_sum" ]; then
    avg_time=$(echo "scale=3; $running_sum / $test_count" | bc)
    echo "Average compilation time (first 5): ${avg_time}s"
fi

echo ""
echo "Results saved to: $RESULTS_FILE"
echo ""
echo "Next steps:"
echo "1. Review results for any failures"
echo "2. Compare with baseline metrics"
echo "3. Identify areas needing optimization"
