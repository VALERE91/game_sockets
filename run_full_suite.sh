#!/bin/bash

# ==============================================================================
# Game Network Protocol Benchmark Suite
# ==============================================================================
# This script runs the server/client pair for UDP, TCP, QUIC, and GNS under
# various network conditions simulated via 'tc' (Traffic Control).
#
# USAGE: sudo ./run_full_suite.sh
# ==============================================================================

# --- Configuration ---
DURATION=30                     # Duration of each test in seconds
PROTOCOLS=("udp" "tcp" "quic" "gns")
RESULTS_DIR="benchmark_results"
SERVER_BIN="./target/release/server"
CLIENT_BIN="./target/release/client"

# Check for Root (Required for 'tc')
if [ "$EUID" -ne 0 ]; then
  echo "Please run as root (needed for network emulation)."
  exit 1
fi

# Check for binaries
if [ ! -f "$SERVER_BIN" ] || [ ! -f "$CLIENT_BIN" ]; then
    echo "Binaries not found. Building project..."
    RUSTFLAGS="-C target-cpu=native" cargo build --release
    if [ $? -ne 0 ]; then
        echo "Build failed."
        exit 1
    fi
fi

# Create Results Directory
mkdir -p "$RESULTS_DIR"

# Cleans up any existing network emulation
reset_netem() {
    tc qdisc del dev lo root 2>/dev/null
}

# Runs a single benchmark test
# $1: Scenario Name (for file naming)
# $2: TC Command Description (for logging)
run_test_case() {
    SCENARIO_NAME=$1
    SCENARIO_DESC=$2

    echo "----------------------------------------------------------------"
    echo "SCENARIO: $SCENARIO_NAME"
    echo "CONFIG:   $SCENARIO_DESC"
    echo "----------------------------------------------------------------"

    # Create directory for this scenario
    SCENARIO_DIR="$RESULTS_DIR/$SCENARIO_NAME"
    mkdir -p "$SCENARIO_DIR"

    # Hardware Isolation Configuration
    SERVER_CORE=2
    CLIENT_CORE=3
    CURRENT_PORT=8000 # Start port

    for PROTO in "${PROTOCOLS[@]}"; do
        echo "  > Testing Protocol: ${PROTO^^} ..."

        # 1. Start Server in Background
        $SERVER_BIN $PROTO --port $CURRENT_PORT > /dev/null 2>&1 &
        SERVER_PID=$!

        # Give server a moment to bind
        sleep 1

        # 2. Run Client
        OUTPUT_FILE="$SCENARIO_DIR/${PROTO}.csv"
        $CLIENT_BIN $PROTO \
                    --ip 127.0.0.1 \
                    --port $CURRENT_PORT \
                    --duration $DURATION \
                    --results "$OUTPUT_FILE" > /dev/null

        # 3. Cleanup Server
        kill $SERVER_PID 2>/dev/null
        wait $SERVER_PID 2>/dev/null

        echo "    Done. Results: $OUTPUT_FILE"
        sleep 1 # Cool down

        # Increment port to prevent TCP TIME_WAIT collisions
        CURRENT_PORT=$((CURRENT_PORT + 1))
    done
    echo ""
}

# ==============================================================================
# 1. Baseline Test (No Emulation)
# ==============================================================================
reset_netem
run_test_case "01_baseline" "No Network Emulation (Localhost)"

# ==============================================================================
# 2. Latency Tests (Jitter included)
# Note: On loopback, traffic hits the interface twice.
# 'delay 10ms' usually results in ~20ms RTT.
# ==============================================================================

# 10ms +- 1ms
reset_netem
tc qdisc add dev lo root netem delay 10ms 1ms distribution normal
run_test_case "02_latency_10ms" "Latency: 10ms +/- 1ms"

# 30ms +- 5ms
reset_netem
tc qdisc add dev lo root netem delay 30ms 5ms distribution normal
run_test_case "03_latency_30ms" "Latency: 30ms +/- 5ms"

# 50ms +- 10ms
reset_netem
tc qdisc add dev lo root netem delay 50ms 10ms distribution normal
run_test_case "04_latency_50ms" "Latency: 50ms +/- 10ms"

# ==============================================================================
# 3. Packet Loss Tests
# ==============================================================================

# 1% Loss
reset_netem
tc qdisc add dev lo root netem loss 1%
run_test_case "05_loss_1p" "Packet Loss: 1%"

# 2% Loss
reset_netem
tc qdisc add dev lo root netem loss 2%
run_test_case "06_loss_2p" "Packet Loss: 2%"

# 5% Loss
reset_netem
tc qdisc add dev lo root netem loss 5%
run_test_case "07_loss_5p" "Packet Loss: 5%"

# ==============================================================================
# 4. Full Real-World Tests
# Combinations of Latency, Jitter, and Loss
# ==============================================================================

# Excellent (Fiber): Low latency, negligible jitter, no loss
reset_netem
tc qdisc add dev lo root netem delay 5ms 1ms distribution normal
run_test_case "08_full_fiber" "Excellent (Fiber): 5ms delay, 1ms jitter"

# Good (Cable/DSL): Moderate latency, some jitter, minor loss (0.1%)
# Note: tc allows chaining commands or complex args
reset_netem
tc qdisc add dev lo root netem delay 30ms 5ms distribution normal loss 0.1%
run_test_case "09_full_good" "Good Connection: 30ms delay, 5ms jitter, 0.1% loss"

# Bad (Unstable WiFi/4G): High latency, high jitter, significant loss (2%)
reset_netem
tc qdisc add dev lo root netem delay 80ms 20ms distribution normal loss 2%
run_test_case "10_full_bad" "Bad Connection: 80ms delay, 20ms jitter, 2% loss"

# ==============================================================================
# Cleanup & Summary
# ==============================================================================
reset_netem
echo "================================================================"
echo "All tests completed."
echo "Results saved in: $RESULTS_DIR"
echo "You can process individual files using: ./target/release/stats <file>"
echo "================================================================"
