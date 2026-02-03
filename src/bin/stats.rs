mod utils;

use std::env;
use std::error::Error;
use std::fs::File;
use std::path::Path;
use crate::utils::BenchmarkRecord;

fn main() -> Result<(), Box<dyn Error>> {
    // Parse CLI Args
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: cargo run --bin stats <csv_file>");
        return Ok(());
    }
    let file_path = &args[1];

    // Load Data
    println!("Loading data from {}...", file_path);
    let mut reader = csv::Reader::from_path(Path::new(file_path))?;
    let mut records: Vec<BenchmarkRecord> = Vec::new();

    for result in reader.deserialize() {
        let record: BenchmarkRecord = result?;
        records.push(record);
    }

    if records.is_empty() {
        println!("No records found.");
        return Ok(());
    }

    // 3. Compute Statistics
    println!("Computing statistics for {} packets...", records.len());
    print_separator();

    // Global Stats
    compute_stats("Global", &records);

    // Per-Stream Stats (e.g. Stream 1 Unreliable vs Stream 2 Reliable)
    let streams: Vec<u16> = records.iter().map(|r| r.stream_id).collect::<std::collections::HashSet<_>>().into_iter().collect();
    for stream in streams {
        let stream_records: Vec<&BenchmarkRecord> = records.iter().filter(|r| r.stream_id == stream).collect();
        // Since we filtered references, we need to map them to calculate stats.
        // For simplicity in this script, we can just clone or adapt the function to take references.
        // Let's adapt the records to a temporary vector for the function.
        // (A cleaner way is making compute_stats generic, but this is fine for a script)
        let stream_records_owned: Vec<BenchmarkRecord> = stream_records.into_iter().map(|r| BenchmarkRecord {
            recv_timestamp: r.recv_timestamp,
            packet_id: r.packet_id,
            stream_id: r.stream_id,
            rtt_us: r.rtt_us,
            payload_size: r.payload_size
        }).collect();

        print_separator();
        compute_stats(&format!("Stream {}", stream), &stream_records_owned);
    }

    Ok(())
}

fn compute_stats(label: &str, records: &[BenchmarkRecord]) {
    if records.is_empty() { return; }

    // --- A. Reliability ---
    // We assume IDs should be sequential.
    // Loss = (MaxID - MinID + 1) - TotalReceived
    let min_id = records.iter().map(|r| r.packet_id).min().unwrap();
    let max_id = records.iter().map(|r| r.packet_id).max().unwrap();
    let total_received = records.len() as u64;
    let expected_sent = max_id - min_id + 1;
    let lost_packets = expected_sent.saturating_sub(total_received);
    let loss_rate = (lost_packets as f64 / expected_sent as f64) * 100.0;

    // Out of Order (simple check: how many times is current_id < prev_id in arrival order?)
    // Note: This assumes the CSV is written in ARRIVAL order.
    let mut out_of_order_count = 0;
    let mut max_seen = 0;
    for r in records {
        if r.packet_id < max_seen {
            out_of_order_count += 1;
        } else {
            max_seen = r.packet_id;
        }
    }

    // --- B. Throughput ---
    let min_time = records.iter().map(|r| r.recv_timestamp).min().unwrap();
    let max_time = records.iter().map(|r| r.recv_timestamp).max().unwrap();
    let duration_us = max_time - min_time;
    let duration_sec = duration_us as f64 / 1_000_000.0;

    let total_bytes: usize = records.iter().map(|r| r.payload_size).sum();
    let goodput_mbps = (total_bytes as f64 * 8.0) / (duration_us as f64) * 1_000_000.0 / 1_000_000.0; // bits/us -> Mbps
    let pps = total_received as f64 / duration_sec;

    // --- C. Latency (RTT) ---
    // We need a sorted vector for percentiles
    let mut rtts: Vec<u64> = records.iter().map(|r| r.rtt_us).collect();
    rtts.sort_unstable();

    let min_rtt = rtts.first().unwrap();
    let max_rtt = rtts.last().unwrap();
    let sum_rtt: u64 = rtts.iter().sum();
    let avg_rtt = sum_rtt as f64 / total_received as f64;

    // StdDev (Jitter)
    let variance = rtts.iter().map(|val| {
        let diff = avg_rtt - (*val as f64);
        diff * diff
    }).sum::<f64>() / total_received as f64;
    let jitter = variance.sqrt();

    // Percentiles
    let p50 = percentile(&rtts, 0.50);
    let p90 = percentile(&rtts, 0.90);
    let p95 = percentile(&rtts, 0.95);
    let p99 = percentile(&rtts, 0.99);
    let p999 = percentile(&rtts, 0.999); // "Three Nines" - critical for pro gaming

    // --- OUTPUT ---
    println!("STATS FOR: {}", label.to_uppercase());
    println!("--------------------------------------------------");
    println!("Reliability:");
    println!("  Total Sent (Est):   {:<10} pkts", expected_sent);
    println!("  Total Received:     {:<10} pkts", total_received);
    println!("  Packet Loss:        {:<10} pkts ({:.2}%)", lost_packets, loss_rate);
    println!("  Out of Order:       {:<10} pkts", out_of_order_count);
    println!("");
    println!("Throughput:");
    println!("  Duration:           {:<10.2} s", duration_sec);
    println!("  Packet Rate:        {:<10.0} pps", pps);
    println!("  Goodput:            {:<10.2} Mbps", goodput_mbps);
    println!("");
    println!("Latency (RTT):");
    println!("  Min:                {:<10} µs", min_rtt);
    println!("  Avg:                {:<10.0} µs", avg_rtt);
    println!("  Max:                {:<10} µs", max_rtt);
    println!("  Jitter (StdDev):    {:<10.2} µs", jitter);
    println!("");
    println!("Percentiles (The 'Real' Lag):");
    println!("  P50 (Median):       {:<10} µs", p50);
    println!("  P90:                {:<10} µs", p90);
    println!("  P95:                {:<10} µs", p95);
    println!("  P99:                {:<10} µs", p99);
    println!("  P99.9:              {:<10} µs", p999);
}

fn percentile(sorted_data: &[u64], percentile: f64) -> u64 {
    if sorted_data.is_empty() { return 0; }
    let index = (percentile * (sorted_data.len() - 1) as f64) as usize;
    sorted_data[index]
}

fn print_separator() {
    println!("\n==================================================\n");
}