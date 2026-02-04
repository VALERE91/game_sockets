# Game Network Protocol Benchmark

A high-performance, asynchronous benchmarking suite written in Rust designed to evaluate transport protocols for real-time multiplayer games.

This tool simulates realistic game traffic patterns—mixing high-frequency unreliable data (e.g., player movement) with lower-frequency reliable data (e.g., game state)—to measure latency, jitter, and packet loss under various network conditions.

> **Note:** For a deep dive into the methodology, performance analysis, and results, please refer to the accompanying scientific paper: *[Insert Paper Title Here]* (Link pending).

## Supported Protocols

* **UDP:** Raw datagrams.
* **TCP:** Standard reliable stream (for baseline comparison).
* **QUIC:** Modern encrypted transport based on `quinn` (IETF QUIC).
* **GNS:** Valve's **GameNetworkingSockets** (used in *Counter-Strike 2*, *Dota 2*).

## Traffic Pattern

Unlike standard bandwidth tools (like `iperf`), this benchmark simulates a real game loop:

1.  **60Hz Unreliable Stream:** Simulates movement/physics data (fire-and-forget).
2.  **20Hz Reliable Stream:** Simulates RPCs, and critical game state updates.

### 1. Build
Ensure you have the Rust toolchain installed (1.70+).

```bash
cargo build --release
```

### 2. Run the Server
Start the server in one terminal. You must specify the protocol to listen on.

```bash
# Options: udp, tcp, quic, gns
./target/release/server --protocol gns --port 8080
```

### 3. Run the Client
Start the client in another terminal (or another machine). This example runs a 30-second benchmark connecting to localhost.

```bash
./target/release/client --protocol gns --ip 127.0.0.1 --port 8080 --duration 30 --results gns_test.csv
```

### 4. Analyze the Results
The client generates a CSV file containing raw timestamps for every packet. Use the included stats tool to generate a summary.

```bash
./target/release/stats gns_test.csv
```

Output Example:

```txt
STATS FOR: STREAM 1 [Unreliable]
--------------------------------------------------
Reliability:
  Packet Loss:        0 pkts (0.00%)

Latency (RTT):
  Avg:                12 ms
  Jitter:             0.45 ms
  P99:                14 ms
```

## Requirements

- Rust: Stable toolchain (1.70+ recommended).

- Linux/macOS: Recommended for accurate network emulation (netem/tc).

- Dependencies (GNS): Building the Valve GNS wrapper requires C++ build tools:
    - Debian/Ubuntu: `sudo apt install cmake clang libssl-dev libprotobuf-dev protobuf-compiler`
    - macOS: `brew install cmake protobuf llvm`

## License

MIT
