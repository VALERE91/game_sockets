mod utils;

use std::env;
use std::error::Error;
use std::path::Path;
use std::collections::HashMap;
use serde::Serialize;
use crate::utils::BenchmarkRecord;

const RELIABILITY_MASK: u16 = 0b11;
const ORDERING_MASK: u16 = 0b10;

#[derive(Serialize)]
struct BenchmarkReport {
    global: ReliabilityStats,
    streams: Vec<StreamReport>,
}

#[derive(Serialize)]
struct StreamReport {
    stream_id: u16,
    stream_index: u16,
    stream_type: String,
    stats: ReliabilityStats,
}

#[derive(Serialize)]
struct ReliabilityStats {
    sent: u64,
    received: u64,
    lost: u64,
    loss_rate_percent: f64,
    throughput: ThroughputStats,
    latency: LatencyStats,
}

#[derive(Serialize)]
struct ThroughputStats {
    duration_sec: f64,
    packet_rate_pps: f64,
    goodput_mbps: f64,
}

#[derive(Serialize)]
struct LatencyStats {
    min_us: u64,
    avg_us: f64,
    max_us: u64,
    jitter_us: f64,
    p50_us: u64,
    p90_us: u64,
    p95_us: u64,
    p99_us: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: cargo run --bin stats <csv_file>");
        return Ok(());
    }
    let file_path = &args[1];

    let mut reader = csv::Reader::from_path(Path::new(file_path))?;
    let mut records: Vec<BenchmarkRecord> = Vec::new();

    for result in reader.deserialize() {
        records.push(result?);
    }

    if records.is_empty() {
        eprintln!("No records found.");
        return Ok(());
    }

    // Group by Stream
    let mut streams: HashMap<u16, Vec<&BenchmarkRecord>> = HashMap::new();
    for r in &records {
        streams.entry(r.stream_id).or_default().push(r);
    }

    // Compute Per-Stream Stats
    let mut stream_reports = Vec::new();
    let mut stream_ids: Vec<u16> = streams.keys().cloned().collect();
    stream_ids.sort();

    // Track globals manually to ensure accurate "total sent" calculation
    let mut global_sent = 0;
    let mut global_received = 0;
    let mut global_lost = 0;

    for stream_id in stream_ids {
        let stream_records = streams.get(&stream_id).unwrap();

        // Calculate Sent/Lost based on sequence gaps
        let min_id = stream_records.iter().map(|r| r.packet_id).min().unwrap_or(0);
        let max_id = stream_records.iter().map(|r| r.packet_id).max().unwrap_or(0);
        let received = stream_records.len() as u64;
        let sent = if received > 0 { max_id - min_id + 1 } else { 0 };
        let lost = sent.saturating_sub(received);

        global_sent += sent;
        global_received += received;
        global_lost += lost;

        // Decode Stream Info
        let index = stream_id >> 2;
        let is_ordered = (stream_id & ORDERING_MASK) != 0;
        let is_reliable_flag = (stream_id & RELIABILITY_MASK) == RELIABILITY_MASK;
        let type_label = if is_reliable_flag { "Reliable" } else if is_ordered { "Ordered" } else { "Unreliable" };

        let stats = calculate_metrics(sent, received, lost, stream_records);

        stream_reports.push(StreamReport {
            stream_id,
            stream_index: index,
            stream_type: type_label.to_string(),
            stats,
        });
    }

    // Compute Global Stats
    // We pass ALL records to calculate aggregate latency/throughput
    let all_records_refs: Vec<&BenchmarkRecord> = records.iter().collect();
    let global_stats = calculate_metrics(global_sent, global_received, global_lost, &all_records_refs);

    // Output JSON
    let report = BenchmarkReport {
        global: global_stats,
        streams: stream_reports,
    };

    let json = serde_json::to_string_pretty(&report)?;
    println!("{}", json);

    Ok(())
}

fn calculate_metrics(sent: u64, received: u64, lost: u64, records: &[&BenchmarkRecord]) -> ReliabilityStats {
    let loss_rate = if sent > 0 { (lost as f64 / sent as f64) * 100.0 } else { 0.0 };

    if records.is_empty() {
        return ReliabilityStats {
            sent, received, lost, loss_rate_percent: loss_rate,
            throughput: ThroughputStats { duration_sec: 0.0, packet_rate_pps: 0.0, goodput_mbps: 0.0 },
            latency: LatencyStats { min_us: 0, avg_us: 0.0, max_us: 0, jitter_us: 0.0, p50_us: 0, p90_us: 0, p95_us: 0, p99_us: 0 },
        };
    }

    // --- Throughput ---
    let min_time = records.iter().map(|r| r.recv_timestamp).min().unwrap();
    let max_time = records.iter().map(|r| r.recv_timestamp).max().unwrap();
    let duration_us = max_time - min_time;
    let duration_sec = duration_us as f64 / 1_000_000.0;

    let total_bytes: usize = records.iter().map(|r| r.payload_size).sum();
    let goodput_mbps = if duration_us > 0 {
        (total_bytes as f64 * 8.0) / (duration_us as f64)
    } else { 0.0 };

    let pps = if duration_sec > 0.0 { received as f64 / duration_sec } else { 0.0 };

    // --- Latency (RTT) ---
    let mut rtts: Vec<u64> = records.iter().map(|r| r.rtt_us).collect();
    rtts.sort_unstable();

    let min_rtt = *rtts.first().unwrap_or(&0);
    let max_rtt = *rtts.last().unwrap_or(&0);
    let sum_rtt: u64 = rtts.iter().sum();
    let avg_rtt = sum_rtt as f64 / received as f64;

    // Jitter
    let variance = rtts.iter().map(|val| {
        let diff = avg_rtt - (*val as f64);
        diff * diff
    }).sum::<f64>() / received as f64;
    let jitter = variance.sqrt();

    ReliabilityStats {
        sent,
        received,
        lost,
        loss_rate_percent: loss_rate,
        throughput: ThroughputStats {
            duration_sec,
            packet_rate_pps: pps,
            goodput_mbps,
        },
        latency: LatencyStats {
            min_us: min_rtt,
            avg_us: avg_rtt,
            max_us: max_rtt,
            jitter_us: jitter,
            p50_us: percentile(&rtts, 0.50),
            p90_us: percentile(&rtts, 0.90),
            p95_us: percentile(&rtts, 0.95),
            p99_us: percentile(&rtts, 0.99),
        }
    }
}

fn percentile(sorted_data: &[u64], percentile: f64) -> u64 {
    if sorted_data.is_empty() { return 0; }
    let index = (percentile * (sorted_data.len() - 1) as f64) as usize;
    sorted_data[index]
}