mod utils;

use utils::BenchmarkPacket;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;
use tracing::{debug, info, warn};
use game_sockets::{GameNetworkEvent, GamePeer, GameSocketError};
use game_sockets::protocols::UdpProtocol;

fn main() -> Result<(), GameSocketError>{
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let protocol = UdpProtocol::new();
    let mut client = GamePeer::new(protocol);
    client.connect("0.0.0.0", 8080)?;

    let mut need_stop: bool = false;

    // State
    let mut server_id = None;

    let mut packet_seq_id = 0u64;
    // Track stats
    let mut sent_packets = std::collections::HashSet::new();

    // Timers
    let mut last_60hz_tick = Instant::now();
    let mut last_20hz_tick = Instant::now();

    let interval_60hz = Duration::from_micros(16666); // ~16.6 ms
    let interval_20hz = Duration::from_millis(50);    // 50 ms

    loop {
        while let Some(event) = client.poll()? {
            match event {
                GameNetworkEvent::Connected(connection) => {
                    info!("Connected to server: {:?}", connection);
                    server_id = Some(connection);
                }
                GameNetworkEvent::Disconnected(connection) => {
                    info!("Disconnected from server: {:?}", connection);
                    server_id = None;
                    need_stop = true;
                    break;
                }
                GameNetworkEvent::Message { connection, stream: _stream, data } => {
                    if let Some(packet) = BenchmarkPacket::from_bytes(data) {

                        // 2. Compute Latency (RTT)
                        let now_micros = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64;
                        let rtt_micros = now_micros.saturating_sub(packet.timestamp);

                        debug!("Packet [{}] RTT: {} µs | Payload: {} bytes",
                                 packet.id, rtt_micros, packet.payload.len());

                        // 3. Mark as received for Drop calculation
                        sent_packets.remove(&packet.id);
                    }  else {
                        warn!("Received invalid packet from server: {:?}", connection);
                    }
                }
                GameNetworkEvent::Error { .. } => {}
                GameNetworkEvent::StreamCreated(_) => {}
                GameNetworkEvent::StreamClosed(_) => {}
            }
        }

        // --- 2. UPDATE PHASE (Send logic) ---
        if let Some(conn) = server_id {
            let now = Instant::now();

            // 60Hz Logic (Stream 1 - Unreliable)
            if now.duration_since(last_60hz_tick) >= interval_60hz {
                packet_seq_id += 1;
                // Create packet with 1000 bytes of random data
                let packet = BenchmarkPacket::new(packet_seq_id, 1000);
                // Store ID to check for drops later (e.g., check set size every second)
                sent_packets.insert(packet_seq_id);
                // Send
                client.send(&conn, 1u16.into(), packet.to_bytes());
                last_60hz_tick = now;
            }

            // 20Hz Logic (Stream 2 - Reliable)
            if now.duration_since(last_20hz_tick) >= interval_20hz {
                packet_seq_id += 1;
                // Create packet with 1000 bytes of random data
                let packet = BenchmarkPacket::new(packet_seq_id, 1000);
                // Store ID to check for drops later (e.g., check set size every second)
                sent_packets.insert(packet_seq_id);
                // Send
                client.send(&conn, 2u16.into(), packet.to_bytes());
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