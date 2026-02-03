mod utils;

use std::collections::HashMap;
use utils::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use clap::{Parser, ValueEnum};
use tokio::time::Instant;
use tracing::{debug, info, warn};
use game_sockets::{GameConnection, GameNetworkEvent, GamePeer, GameSocketError, GameSocketProtocol, GameStream, GameStreamReliability};
use game_sockets::protocols::{TcpProtocol, UdpProtocol};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum TestProtocol {
    Udp,
    Tcp,
    Quic,
    GNS
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct CliArgs{
    #[arg(value_enum)]
    protocol: TestProtocol,
    #[arg(long, short, default_value = "127.0.0.1", help = "IP address of the server to connect to (default: 127.0.0.1)")]
    ip: String,
    #[arg(long, short, default_value = "8080", help = "Port of the server to connect to (default: 8080)")]
    port: u16,
    #[arg(long, short, default_value = "results.csv", help = "The file to write the results to (default: results.csv)")]
    results: String,
    #[arg(long, default_value = "1000", help = "The size of the packets to send (default: 1000)")]
    packet_size: usize,
}

fn main() -> Result<(), GameSocketError>{
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = CliArgs::parse();

    let recorder = MetricsRecorder::new(&args.results);

    let protocol = TcpProtocol::new();
    let mut client = GamePeer::new(protocol);
    client.connect(&args.ip, args.port)?;

    let mut need_stop: bool = false;

    // State
    let mut server_id = None;

    let mut packet_sequences: HashMap<GameStream, u64> = HashMap::new();

    // Timers
    let mut last_60hz_tick = Instant::now();
    let mut unreliable_game_stream = Option::<GameStream>::None;
    let mut last_20hz_tick = Instant::now();
    let mut reliable_game_stream = Option::<GameStream>::None;

    let interval_60hz = Duration::from_micros(16666); // ~16.6 ms
    let interval_20hz = Duration::from_millis(50);    // 50 ms

    loop {
        while let Some(event) = client.poll()? {
            match event {
                GameNetworkEvent::Connected(connection) => {
                    info!("Connected to server: {:?}", connection);
                    server_id = Some(connection);
                    client.create_stream(connection, GameStreamReliability::Unreliable)?;
                    client.create_stream(connection, GameStreamReliability::Reliable)?;
                }
                GameNetworkEvent::Disconnected(connection) => {
                    info!("Disconnected from server: {:?}", connection);
                    server_id = None;
                    need_stop = true;
                    break;
                }
                GameNetworkEvent::Message { connection, stream, data } => {
                    if let Some(packet) = BenchmarkPacket::from_bytes(data) {

                        // Compute Latency (RTT)
                        let now_micros = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64;
                        let rtt_micros = now_micros.saturating_sub(packet.timestamp);

                        debug!("Packet [{}] RTT: {} µs | Payload: {} bytes",
                                 packet.id, rtt_micros, packet.payload.len());

                        recorder.record(BenchmarkRecord {
                            packet_id: packet.id,
                            stream_id: stream.stream_id,
                            rtt_us: rtt_micros,
                            payload_size: packet.payload.len(),
                            recv_timestamp: now_micros,
                        });
                    }  else {
                        warn!("Received invalid packet from server: {:?}", connection);
                    }
                }
                GameNetworkEvent::Error { connection: _connection, inner } => {
                    warn!("Error from server: {:?}", inner);
                }
                GameNetworkEvent::StreamCreated(stream) => {
                    if stream.is_reliable() {
                        reliable_game_stream = Some(stream);
                    } else {
                        unreliable_game_stream = Some(stream);
                    }
                }
                GameNetworkEvent::StreamClosed(stream) => {
                    if stream.is_reliable() {
                        reliable_game_stream = None;
                    } else {
                        unreliable_game_stream = None;
                    }
                }
            }
        }

        // Sending packets
        if let Some(conn) = server_id {
            let now = Instant::now();

            // 60Hz Logic (Stream 1 - Unreliable)
            if now.duration_since(last_60hz_tick) >= interval_60hz {
                let Some(ref stream) = unreliable_game_stream else { continue };
                send_packet(&conn, &stream, &mut client, args.packet_size, &mut packet_sequences);
                last_60hz_tick = now;
            }

            // 20Hz Logic (Stream 2 - Reliable)
            if now.duration_since(last_20hz_tick) >= interval_20hz {
                let Some(ref stream) = reliable_game_stream else { continue };
                send_packet(&conn, &stream, &mut client, args.packet_size, &mut packet_sequences);
                last_20hz_tick = now;
            }
        } else {
            debug!("Not connected to server");
        }

        if need_stop {
            break;
        }
        std::thread::yield_now();
    }

    info!("Finished");
    Ok(())
}

fn send_packet<T : GameSocketProtocol>(conn: &GameConnection, stream: &GameStream, client: &mut GamePeer<T>,
               padding: usize, packet_sequences: &mut HashMap<GameStream, u64>) {
    let packet_seq_id = packet_sequences.entry(stream.clone()).or_insert(0);
    let packet = BenchmarkPacket::new(*packet_seq_id, padding);
    client.send(&conn, stream, packet.to_bytes());
    *packet_seq_id += 1;
}