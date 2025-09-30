#!/bin/bash

# Benchmark script to compare ccusage (Node.js) vs ccusage-statusline-rs (Rust)

set -e

RUNS=10
TEST_DATA="/mnt/bcachefs/home/jerome/GIT/ccusage/apps/ccusage/test/statusline-test.json"

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "=========================================="
echo "  Performance Comparison Benchmark"
echo "=========================================="
echo ""
echo "Runs per tool: $RUNS"
echo "Test data: $TEST_DATA"
echo ""

# Ensure test data exists
if [ ! -f "$TEST_DATA" ]; then
    echo "Error: Test data not found at $TEST_DATA"
    exit 1
fi

# Node.js ccusage benchmark
echo -e "${BLUE}Benchmarking Node.js ccusage...${NC}"
NODE_TOTAL=0
for i in $(seq 1 $RUNS); do
    RESULT=$( { time cat "$TEST_DATA" | node /mnt/bcachefs/home/jerome/GIT/ccusage/apps/ccusage/dist/index.js statusline --visual-burn-rate emoji > /dev/null; } 2>&1 | grep real | awk '{print $2}' )
    # Convert to milliseconds
    MS=$(echo "$RESULT" | awk -F'[ms]' '{print ($1 * 60000) + ($2 * 1000) + $3}')
    NODE_TOTAL=$(echo "$NODE_TOTAL + $MS" | bc)
    echo "  Run $i: ${MS}ms"
done
NODE_AVG=$(echo "scale=2; $NODE_TOTAL / $RUNS" | bc)

echo ""

# Rust ccusage-statusline-rs benchmark
echo -e "${BLUE}Benchmarking Rust ccusage-statusline-rs...${NC}"
RUST_TOTAL=0
for i in $(seq 1 $RUNS); do
    RESULT=$( { time cat "$TEST_DATA" | ./target/release/ccusage-statusline-rs > /dev/null; } 2>&1 | grep real | awk '{print $2}' )
    # Convert to milliseconds
    MS=$(echo "$RESULT" | awk -F'[ms]' '{print ($1 * 60000) + ($2 * 1000) + $3}')
    RUST_TOTAL=$(echo "$RUST_TOTAL + $MS" | bc)
    echo "  Run $i: ${MS}ms"
done
RUST_AVG=$(echo "scale=2; $RUST_TOTAL / $RUNS" | bc)

echo ""
echo "=========================================="
echo "  Results"
echo "=========================================="
echo ""
echo -e "${YELLOW}Node.js ccusage:${NC}"
echo "  Average: ${NODE_AVG}ms"
echo ""
echo -e "${GREEN}Rust ccusage-statusline-rs:${NC}"
echo "  Average: ${RUST_AVG}ms"
echo ""

# Calculate speedup
SPEEDUP=$(echo "scale=2; $NODE_AVG / $RUST_AVG" | bc)
IMPROVEMENT=$(echo "scale=1; (($NODE_AVG - $RUST_AVG) / $NODE_AVG) * 100" | bc)

echo -e "${GREEN}Performance Improvement:${NC}"
echo "  Speedup: ${SPEEDUP}x faster"
echo "  Improvement: ${IMPROVEMENT}% faster"
echo ""