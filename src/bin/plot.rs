use std::collections::HashMap;
use plotters::prelude::*;
use serde::Deserialize;
use std::fs::File;
use std::io::BufReader;
use std::ops::Range;
use std::string::ToString;

#[derive(Deserialize)]
struct GlobalStats {
    latency: LatencyStats,
}

#[derive(Deserialize)]
struct LatencyStats {
    avg_us: f64,
    p99_us: f64,
}

#[derive(Deserialize)]
struct BenchmarkJson {
    global: GlobalStats,
}

const COLOR_UDP: RGBColor = RGBColor(127, 127, 127); // Gray
const COLOR_TCP: RGBColor = RGBColor(214, 39, 40);   // Red
const COLOR_QUIC: RGBColor = RGBColor(31, 119, 180); // Blue
const COLOR_GNS: RGBColor = RGBColor(44, 160, 44);   // Green

const UDP_NAME: &str = "UDP";
const TCP_NAME: &str = "TCP";
const QUIC_NAME: &str = "QUIC";
const GNS_NAME: &str = "GNS";

// Helper to load a specific file
fn load_latency(path: &str) -> (f64, f64) {
    let file = File::open(path).unwrap_or_else(|_| panic!("Failed to open {}", path));
    let reader = BufReader::new(file);
    let data: BenchmarkJson = serde_json::from_reader(reader).expect("JSON Parse Error");
    (data.global.latency.avg_us / 1000.0, data.global.latency.p99_us / 1000.0)
}

fn load_loss(path: &str) -> Vec<(f64, &str, String)> {
    vec![
        (0.0, UDP_NAME, format!("{}/01_baseline/udp.json", path)),
        (0.0, TCP_NAME, format!("{}/01_baseline/tcp.json", path)),
        (0.0, QUIC_NAME, format!("{}/01_baseline/quic.json", path)),
        (0.0, GNS_NAME, format!("{}/01_baseline/gns.json", path)),

        (1.0, UDP_NAME, format!("{}/05_loss_1p/udp.json", path)),
        (1.0, TCP_NAME, format!("{}/05_loss_1p/tcp.json", path)),
        (1.0, QUIC_NAME, format!("{}/05_loss_1p/quic.json", path)),
        (1.0, GNS_NAME, format!("{}/05_loss_1p/gns.json", path)),

        (2.0, UDP_NAME, format!("{}/06_loss_2p/udp.json", path)),
        (2.0, TCP_NAME, format!("{}/06_loss_2p/tcp.json", path)),
        (2.0, QUIC_NAME, format!("{}/06_loss_2p/quic.json", path)),
        (2.0, GNS_NAME, format!("{}/06_loss_2p/gns.json", path)),

        (5.0, UDP_NAME, format!("{}/07_loss_5p/udp.json", path)),
        (5.0, TCP_NAME, format!("{}/07_loss_5p/tcp.json", path)),
        (5.0, QUIC_NAME, format!("{}/07_loss_5p/quic.json", path)),
        (5.0, GNS_NAME, format!("{}/07_loss_5p/gns.json", path)),
    ]
}

fn load_loss_no_tcp(path: &str) -> Vec<(f64, &str, String)> {
    vec![
        (0.0, UDP_NAME, format!("{}/01_baseline/udp.json", path)),
        (0.0, QUIC_NAME, format!("{}/01_baseline/quic.json", path)),
        (0.0, GNS_NAME, format!("{}/01_baseline/gns.json", path)),

        (1.0, UDP_NAME, format!("{}/05_loss_1p/udp.json", path)),
        (1.0, QUIC_NAME, format!("{}/05_loss_1p/quic.json", path)),
        (1.0, GNS_NAME, format!("{}/05_loss_1p/gns.json", path)),

        (2.0, UDP_NAME, format!("{}/06_loss_2p/udp.json", path)),
        (2.0, QUIC_NAME, format!("{}/06_loss_2p/quic.json", path)),
        (2.0, GNS_NAME, format!("{}/06_loss_2p/gns.json", path)),

        (5.0, UDP_NAME, format!("{}/07_loss_5p/udp.json", path)),
        (5.0, QUIC_NAME, format!("{}/07_loss_5p/quic.json", path)),
        (5.0, GNS_NAME, format!("{}/07_loss_5p/gns.json", path)),
    ]
}

fn load_latency_data(path: &str) -> Vec<(f64, &str, String)> {
    vec![
        (0.0, UDP_NAME, format!("{}/01_baseline/udp.json", path)),
        (0.0, TCP_NAME, format!("{}/01_baseline/tcp.json", path)),
        (0.0, QUIC_NAME, format!("{}/01_baseline/quic.json", path)),
        (0.0, GNS_NAME, format!("{}/01_baseline/gns.json", path)),

        (10.0, UDP_NAME, format!("{}/02_latency_10ms/udp.json", path)),
        (10.0, TCP_NAME, format!("{}/02_latency_10ms/tcp.json", path)),
        (10.0, QUIC_NAME, format!("{}/02_latency_10ms/quic.json", path)),
        (10.0, GNS_NAME, format!("{}/02_latency_10ms/gns.json", path)),

        (30.0, UDP_NAME, format!("{}/03_latency_30ms/udp.json", path)),
        (30.0, TCP_NAME, format!("{}/03_latency_30ms/tcp.json", path)),
        (30.0, QUIC_NAME, format!("{}/03_latency_30ms/quic.json", path)),
        (30.0, GNS_NAME, format!("{}/03_latency_30ms/gns.json", path)),

        (50.0, UDP_NAME, format!("{}/04_latency_50ms/udp.json", path)),
        (50.0, TCP_NAME, format!("{}/04_latency_50ms/tcp.json", path)),
        (50.0, QUIC_NAME, format!("{}/04_latency_50ms/quic.json", path)),
        (50.0, GNS_NAME, format!("{}/04_latency_50ms/gns.json", path)),
    ]
}

fn draw_line_chart(path: &str,
                   caption: &str,
                   x_range: Range<f64>,
                   y_range: Range<f64>,
                   x_desc: &str,
                   y_desc: &str,
                   data_color: &Vec<(&str, &RGBColor)>,
                   data: &Vec<(f64, &str, String)>) -> Result<(),()> {
    //Prepare Drawing Area (SVG)
    let root = SVGBackend::new(path, (800, 600)).into_drawing_area();
    root.fill(&WHITE).map_err(|_| ())?;

    // Configure Chart
    let mut chart = ChartBuilder::on(&root)
        .caption(caption, ("sans-serif", 35).into_font())
        .margin(20)
        .x_label_area_size(50)
        .y_label_area_size(70)
        .build_cartesian_2d(x_range, y_range).map_err(|_| ())?; // Axis Ranges: X (0-4%), Y (0-500ms)

    chart.configure_mesh()
        .x_desc(x_desc)
        .y_desc(y_desc)
        .axis_desc_style(("sans-serif", 30)) // Increased font size for axis titles
        .label_style(("sans-serif", 20))     // Increased font size for tick marks
        .draw().map_err(|_| ())?;

    for (name, color) in data_color {
        let series_data: Vec<(f64, f64)> = data.iter()
            .filter(|p| p.1.eq_ignore_ascii_case(name))
            .map(|p| {
                let (_avg, p99) = load_latency(p.2.as_str());
                (p.0, p99)
            })
            .collect();

        chart.draw_series(LineSeries::new(series_data, color.stroke_width(3))).map_err(|_| ())?
            .label(name.to_string())
            .legend(move |(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], color.filled()));
    }

    chart.configure_series_labels()
        .background_style(&WHITE.mix(0.8))
        .border_style(&BLACK)
        .label_font(("sans-serif", 20)) // Increased font size for legend
        .draw()
        .map_err(|_| ())?;

    println!("{}", format!("Saved to {}", path));
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_points_loss = load_loss("benchmark_results");
    let data_points_loss_no_tcp = load_loss_no_tcp("benchmark_results");
    let data_points_latency = load_latency_data("benchmark_results");

    let protocols = vec![
        ("UDP", &COLOR_UDP),
        ("TCP", &COLOR_TCP),
        ("QUIC", &COLOR_QUIC),
        ("GNS", &COLOR_GNS),
    ];

    draw_line_chart("packet_loss_trend_tcp.svg",
                    "Latency vs Packet Loss",
                    0.0..6.0,
                    0.0..900.0,
                    "Packet Loss (%)",
                    "Average Latency (ms)",
                    &protocols,
                    &data_points_loss).unwrap();

    draw_line_chart("latency_trend.svg",
                    "Protocol Latency vs Network Latency",
                    0.0..60.0,
                    0.0..350.0,
                    "Network Latency (ms)",
                    "Protocol Latency (ms)",
                    &protocols,
                    &data_points_latency).unwrap();

    let protocols = vec![
        ("UDP", &COLOR_UDP),
        ("QUIC", &COLOR_QUIC),
        ("GNS", &COLOR_GNS),
    ];

    draw_line_chart("packet_loss_trend_no_tcp.svg",
                    "Latency vs Packet Loss (No TCP)",
                    0.0..6.0,
                    0.0..100.0,
                    "Packet Loss (%)",
                    "Average Latency (ms)",
                    &protocols,
                    &data_points_loss_no_tcp).unwrap();

    // Generate the bar charts
    draw_bars()?;

    Ok(())
}

// Helper to load data
fn load_metric(path: &str, use_p99: bool) -> f64 {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => {
            eprintln!("Warning: Could not open file {}, returning 0.0", path);
            return 0.0;
        }
    };
    let reader = BufReader::new(file);
    let json: BenchmarkJson = serde_json::from_reader(reader).unwrap_or_else(|_| {
        panic!("Failed to parse JSON: {}", path)
    });

    let val_us = if use_p99 {
        json.global.latency.p99_us
    } else {
        json.global.latency.avg_us
    };
    val_us / 1000.0 // Convert us to ms
}

struct ScenarioData {
    name: String,               // e.g., "Excellent"
    data: HashMap<String, f64>, // Protocol -> Value (ms)
}

fn draw_bars() -> Result<(), Box<dyn std::error::Error>> {
    let protocols = vec![UDP_NAME, TCP_NAME, QUIC_NAME, GNS_NAME];
    let condition_names = vec!["Excellent", "Good", "Bad"];

    // Data Loading Logic (Act 3)
    let scenarios = vec![
        ScenarioData {
            name: "Excellent".to_string(),
            data: HashMap::from([
                (UDP_NAME.to_string(),  load_metric("benchmark_results/08_full_fiber/udp.json", true)),
                (TCP_NAME.to_string(),  load_metric("benchmark_results/08_full_fiber/tcp.json", true)),
                (QUIC_NAME.to_string(), load_metric("benchmark_results/08_full_fiber/quic.json", true)),
                (GNS_NAME.to_string(),  load_metric("benchmark_results/08_full_fiber/gns.json", true)),
            ]),
        },
        ScenarioData {
            name: "Good".to_string(),
            data: HashMap::from([
                (UDP_NAME.to_string(),  load_metric("benchmark_results/09_full_good/udp.json", true)),
                (TCP_NAME.to_string(),  load_metric("benchmark_results/09_full_good/tcp.json", true)),
                (QUIC_NAME.to_string(), load_metric("benchmark_results/09_full_good/quic.json", true)),
                (GNS_NAME.to_string(),  load_metric("benchmark_results/09_full_good/gns.json", true)),
            ]),
        },
        ScenarioData {
            name: "Bad".to_string(),
            data: HashMap::from([
                (UDP_NAME.to_string(),  load_metric("benchmark_results/10_full_bad/udp.json", true)),
                (TCP_NAME.to_string(),  load_metric("benchmark_results/10_full_bad/tcp.json", true)),
                (QUIC_NAME.to_string(), load_metric("benchmark_results/10_full_bad/quic.json", true)),
                (GNS_NAME.to_string(),  load_metric("benchmark_results/10_full_bad/gns.json", true)),
            ]),
        },
    ];

    // Calculate max Y for scaling
    let max_val = scenarios.iter()
        .flat_map(|s| s.data.values())
        .fold(0.0f64, |a, &b| a.max(b));
    let y_max = max_val * 1.1; // Add 10% headroom

    // ==========================================
    // DRAWING LOGIC
    // ==========================================
    let root = SVGBackend::new("scenario.svg", (1000, 600)).into_drawing_area();
    root.fill(&WHITE)?;

    // We use a float coordinate system (-0.5 to 2.5) to center the bars perfectly
    let mut chart = ChartBuilder::on(&root)
        .caption("Protocol Performance by Network Condition (P99 Latency)", ("sans-serif", 40).into_font())
        .margin(20)
        .x_label_area_size(60)
        .y_label_area_size(80)
        .build_cartesian_2d(
            -0.5f64..2.5f64, // X: Float range for centering
            0.0..y_max       // Y: 0..Max
        )?;

    chart.configure_mesh()
        .x_labels(3)
        .y_desc("Latency (ms)")
        .axis_desc_style(("sans-serif", 35)) // Increased font
        .label_style(("sans-serif", 25))     // Increased font
        .x_label_formatter(&|v| {
            let idx = v.round();
            if (v - idx).abs() < 0.1 {
                let i = idx as usize;
                if i < condition_names.len() {
                    return condition_names[i].to_string();
                }
            }
            "".to_string()
        })
        .draw()?;

    // Grouped Bar Calculation
    let total_bars = protocols.len();
    let total_width_ratio = 0.8; // Bars take up 80% of the category width
    let bar_width = total_width_ratio / total_bars as f64;

    // Draw Bars
    for (scenario_idx, scenario) in scenarios.iter().enumerate() {
        for (proto_idx, proto) in protocols.iter().enumerate() {
            let val = *scenario.data.get(*proto).unwrap_or(&0.0);

            // Calculate exact X position
            let group_center = scenario_idx as f64;
            let group_start_x = group_center - (total_width_ratio / 2.0);

            let bar_start_x = group_start_x + (proto_idx as f64 * bar_width);
            let bar_end_x = bar_start_x + bar_width;

            let color = match *proto {
                UDP_NAME => COLOR_UDP,
                TCP_NAME => COLOR_TCP,
                QUIC_NAME => COLOR_QUIC,
                GNS_NAME => COLOR_GNS,
                _ => COLOR_GNS,
            };

            // Draw Rectangle
            chart.draw_series(std::iter::once(
                Rectangle::new(
                    [(bar_start_x, 0.0), (bar_end_x, val)],
                    color.filled()
                )
            ))?;
        }
    }

    // ==========================================
    // LEGEND
    // ==========================================
    for proto in protocols {
        let color = match proto {
            UDP_NAME => COLOR_UDP,
            TCP_NAME => COLOR_TCP,
            QUIC_NAME => COLOR_QUIC,
            _ => COLOR_GNS,
        };

        chart.draw_series(std::iter::once(PathElement::new(vec![], color.filled())))?
            .label(proto)
            .legend(move |(x, y)| Rectangle::new([(x, y - 5), (x + 10, y + 5)], color.filled()));
    }

    chart.configure_series_labels()
        .background_style(&WHITE.mix(0.8))
        .border_style(&BLACK)
        .label_font(("sans-serif", 20)) // Increased legend font
        .position(SeriesLabelPosition::UpperLeft)
        .draw()?;

    println!("Saved to scenario.svg");
    Ok(())
}