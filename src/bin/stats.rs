mod utils;

use std::env;
use std::error::Error;
use std::path::Path;
use std::collections::HashMap;
use crate::utils::BenchmarkRecord;

// Protocol Constants (Mirrored from lib.rs for decoding)
const RELIABILITY_MASK: u16 = 0b11;
const ORDERING_MASK: u16 = 0b10;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: cargo run --bin stats <csv_file>");
        return Ok(());
    }
    let file_path = &args[1];

    println!("Loading data from {}...", file_path);
    let mut reader = csv::Reader::from_path(Path::new(file_path))?;
    let mut records: Vec<BenchmarkRecord> = Vec::new();

    for result in reader.deserialize() {
        records.push(result?);
    }

    if records.is_empty() {
        println!("No records found.");
        return Ok(());
    }

    println!("Computing statistics for {} packets...", records.len());
    print_separator();

    // Group by Stream
    let mut streams: HashMap<u16, Vec<&BenchmarkRecord>> = HashMap::new();
    for r in &records {
        streams.entry(r.stream_id).or_default().push(r);
    }

    let mut global_sent = 0;
    let mut global_received = 0;
    let mut global_lost = 0;

    // Compute Per-Stream Stats
    let mut stream_ids: Vec<u16> = streams.keys().cloned().collect();
    stream_ids.sort();

    for stream_id in stream_ids {
        let stream_records = streams.get(&stream_id).unwrap();

        let min_id = stream_records.iter().map(|r| r.packet_id).min().unwrap_or(0);
        let max_id = stream_records.iter().map(|r| r.packet_id).max().unwrap_or(0);

        let received = stream_records.len() as u64;
        // Logic: If we received IDs 5, 6, 8, we know 7 was lost. Total sent = Max - Min + 1.
        let sent = if received > 0 { max_id - min_id + 1 } else { 0 };
        let lost = sent.saturating_sub(received);

        // Accumulate for Global
        global_sent += sent;
        global_received += received;
        global_lost += lost;

        // Decode the Stream ID
        let index = stream_id >> 2;
        let is_ordered = (stream_id & ORDERING_MASK) != 0;
        let is_reliable_flag = (stream_id & RELIABILITY_MASK) == RELIABILITY_MASK;

        let type_label = if is_reliable_flag {
            "Reliable"
        } else if is_ordered {
            "Ordered"
        } else {
            "Unreliable"
        };

        print_separator();
        let label = format!("Stream {} [Index: {}, Type: {}]", stream_id, index, type_label);
        print_stats_report(&label, sent, received, lost, stream_records);
    }

    // 3. Compute Global Stats (Aggregation)
    print_separator();

    // For reliability, we use the SUM of the per-stream stats
    let loss_rate = if global_sent > 0 { (global_lost as f64 / global_sent as f64) * 100.0 } else { 0.0 };

    println!("STATS FOR: GLOBAL (AGGREGATED)");
    println!("--------------------------------------------------");
    println!("Reliability:");
    println!("  Total Sent:         {:<10} pkts", global_sent);
    println!("  Total Received:     {:<10} pkts", global_received);
    println!("  Packet Loss:        {:<10} pkts ({:.2}%)", global_lost, loss_rate);

    // For Latency/Throughput, we can treat all records together
    // Convert Vec<Record> to Vec<&Record> for the helper
    let all_records_refs: Vec<&BenchmarkRecord> = records.iter().collect();
    print_latency_throughput_stats(&all_records_refs);

    Ok(())
}

fn print_stats_report(label: &str, sent: u64, received: u64, lost: u64, records: &[&BenchmarkRecord]) {
    let loss_rate = if sent > 0 { (lost as f64 / sent as f64) * 100.0 } else { 0.0 };

    println!("STATS FOR: {}", label.to_uppercase());
    println!("--------------------------------------------------");
    println!("Reliability:");
    println!("  Total Sent:         {:<10} pkts", sent);
    println!("  Total Received:     {:<10} pkts", received);
    println!("  Packet Loss:        {:<10} pkts ({:.2}%)", lost, loss_rate);

    print_latency_throughput_stats(records);
}

fn print_latency_throughput_stats(records: &[&BenchmarkRecord]) {
    if records.is_empty() { return; }

    // --- Throughput ---
    let min_time = records.iter().map(|r| r.recv_timestamp).min().unwrap();
    let max_time = records.iter().map(|r| r.recv_timestamp).max().unwrap();
    let duration_us = max_time - min_time;
    let duration_sec = duration_us as f64 / 1_000_000.0;

    let total_bytes: usize = records.iter().map(|r| r.payload_size).sum();
    let total_received = records.len() as u64;

    let goodput_mbps = if duration_us > 0 {
        (total_bytes as f64 * 8.0) / (duration_us as f64) // bits per microsecond = megabits per second
    } else { 0.0 };

    let pps = if duration_sec > 0.0 { total_received as f64 / duration_sec } else { 0.0 };

    println!("");
    println!("Throughput:");
    println!("  Duration:           {:<10.2} s", duration_sec);
    println!("  Packet Rate:        {:<10.0} pps", pps);
    println!("  Goodput:            {:<10.2} Mbps", goodput_mbps);

    // --- Latency (RTT) ---
    let mut rtts: Vec<u64> = records.iter().map(|r| r.rtt_us).collect();
    rtts.sort_unstable();

    let min_rtt = rtts.first().unwrap();
    let max_rtt = rtts.last().unwrap();
    let sum_rtt: u64 = rtts.iter().sum();
    let avg_rtt = sum_rtt as f64 / total_received as f64;

    // Jitter
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

    println!("");
    println!("Latency (RTT):");
    println!("  Min:                {:<10} µs", min_rtt);
    println!("  Avg:                {:<10.0} µs", avg_rtt);
    println!("  Max:                {:<10} µs", max_rtt);
    println!("  Jitter:             {:<10.2} µs", jitter);
    println!("");
    println!("Percentiles:");
    println!("  P50:                {:<10} µs", p50);
    println!("  P90:                {:<10} µs", p90);
    println!("  P95:                {:<10} µs", p95);
    println!("  P99:                {:<10} µs", p99);
}

fn percentile(sorted_data: &[u64], percentile: f64) -> u64 {
    if sorted_data.is_empty() { return 0; }
    let index = (percentile * (sorted_data.len() - 1) as f64) as usize;
    sorted_data[index]
}

fn print_separator() {
    println!("\n==================================================\n");
}