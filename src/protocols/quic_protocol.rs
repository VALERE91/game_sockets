use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use uuid::Uuid;
use quinn::{Endpoint, Connection, RecvStream, SendStream};
use quinn::congestion::BbrConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use crate::{BackendCommand, GameNetworkEvent, GameSocketBackend, GameSocketError, GameStream};
use rustls::client::{ServerCertVerified, ServerCertVerifier};
use tracing::error;
use tracing::log::{debug, log};

struct SkipServerVerification;

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
}

fn make_server_config() -> (quinn::ServerConfig, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.serialize_der().unwrap();
    let priv_key = cert.serialize_private_key_der();
    let priv_key = rustls::PrivateKey(priv_key);
    let cert_chain = vec![rustls::Certificate(cert_der.clone())];

    let mut server_config = quinn::ServerConfig::with_single_cert(cert_chain, priv_key).unwrap();

    let mut transport_config = quinn::TransportConfig::default();
    transport_config.max_concurrent_uni_streams(0_u8.into());
    transport_config.max_concurrent_bidi_streams(100_u8.into()); // Support 100 reliable streams
    transport_config.datagram_receive_buffer_size(Some(1024 * 1024));

    // --- GAMING OPTIMIZATIONS ---

    // Switch to BBR (Bottleneck Bandwidth and Round-trip propagation time)
    // Cubic (default) fills buffers until packet loss occurs (bad for latency).
    // BBR models the network to keep buffers empty (great for gaming).
    let bbr_config = BbrConfig::default();
    transport_config.congestion_controller_factory(Arc::new(bbr_config));

    // Disable Datagram Pacing (Critical for "Unreliable" lane)
    transport_config.datagram_send_buffer_size(0); // 0 means "send immediately or drop" for some impls, but larger buffer with BBR is safer.

    // Boost Timers for fast "Lost Packet" detection
    // Default initial RTT is 333ms. Set it to a realistic gaming value (e.g., 15ms).
    transport_config.initial_rtt(Duration::from_millis(15));

    // Keep Alive (Prevent timeouts during loading screens)
    transport_config.keep_alive_interval(Some(Duration::from_secs(1)));

    server_config.transport_config(Arc::new(transport_config));

    (server_config, cert_der)
}

fn make_client_config() -> quinn::ClientConfig {
    let crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();

    let mut client_config = quinn::ClientConfig::new(Arc::new(crypto));
    let mut transport_config = quinn::TransportConfig::default();
    transport_config.datagram_receive_buffer_size(Some(1024 * 1024));
    transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(5)));

    let bbr_config = BbrConfig::default();
    transport_config.congestion_controller_factory(Arc::new(bbr_config));
    transport_config.datagram_send_buffer_size(0);
    transport_config.initial_rtt(Duration::from_millis(15));
    transport_config.keep_alive_interval(Some(Duration::from_secs(1)));

    client_config.transport_config(Arc::new(transport_config));

    client_config
}

pub struct QuicBackend {
    connections: HashMap<Uuid, Connection>,
    send_streams: HashMap<(Uuid, u16), (SendStream, RecvStream)>
}

impl GameSocketBackend for QuicBackend {
    fn run(mut self, mut cmd_rx: mpsc::UnboundedReceiver<BackendCommand>, event_tx: mpsc::UnboundedSender<GameNetworkEvent>) {
        let rt = Runtime::new().expect("Failed to create Tokio runtime");
        rt.block_on(async move {
            let (conn_reg_tx, mut conn_reg_rx) = mpsc::unbounded_channel::<(Uuid, Connection)>();

            loop {
                tokio::select! {
                    Some((uuid, conn)) = conn_reg_rx.recv() => {
                        self.connections.insert(uuid, conn);
                    }

                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            BackendCommand::Bind { addr, port } => {
                                let (server_config, _cert) = make_server_config();
                                let addr = format!("{}:{}", addr, port).parse().unwrap();
                                let endpoint = Endpoint::server(server_config, addr).unwrap();
                                let event_tx = event_tx.clone();
                                let conn_reg_tx = conn_reg_tx.clone(); // Clone for task

                                tokio::spawn(async move {
                                    while let Some(conn) = endpoint.accept().await {
                                        let connection = conn.await.unwrap();
                                        let uuid = Uuid::new_v4();

                                        // Notify Game Thread
                                        let _ = event_tx.send(GameNetworkEvent::Connected(uuid.into()));

                                        // Notify Backend Thread so we can send data back
                                        let _ = conn_reg_tx.send((uuid, connection.clone()));

                                        QuicBackend::spawn_reader(connection, uuid, event_tx.clone());
                                    }
                                });
                            }
                            BackendCommand::Connect { addr, port } => {
                                let client_config = make_client_config();
                                let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
                                endpoint.set_default_client_config(client_config);

                                let remote = format!("{}:{}", addr, port).parse().unwrap();
                                let connection = endpoint.connect(remote, "localhost").unwrap().await.unwrap();
                                let uuid = Uuid::new_v4();

                                self.connections.insert(uuid, connection.clone());
                                let _ = event_tx.send(GameNetworkEvent::Connected(uuid.into()));

                                QuicBackend::spawn_reader(connection, uuid, event_tx.clone());
                            }
                            BackendCommand::Send { connection, stream, data } => {
                                if let Some(conn) = self.connections.get(&connection) {
                                    if stream.is_reliable() {
                                        let key = (connection, stream.stream_id);
                                        let send_stream = if let Some(s) = self.send_streams.get_mut(&key) {
                                            s
                                        } else {
                                            debug!("No stream found for {:?}. Performing lazy creation.", stream.stream_id);
                                            let mut s = conn.open_bi().await.unwrap();
                                            let _ = s.0.write_u16(stream.stream_id).await;
                                            self.send_streams.insert(key, s);
                                            self.send_streams.get_mut(&key).unwrap()
                                        };

                                        let mut frame = BytesMut::with_capacity(4 + data.len());
                                        frame.put_u32(data.len() as u32);
                                        frame.put(data);
                                        match send_stream.0.write_all(&frame).await {
                                            Ok(_) => (),
                                            Err(e)=> {
                                                let _ = event_tx.send(GameNetworkEvent::Error {
                                                    connection: connection.into(),
                                                    inner: GameSocketError::SendFailed{ inner_msg: e.to_string()}
                                                });
                                                error!("Error sending packet: {:?}", e)
                                            }
                                        }
                                    } else {
                                        let mut packet = BytesMut::with_capacity(2 + data.len());
                                        packet.put_u16(stream.stream_id);
                                        packet.put(data);
                                        match conn.send_datagram(packet.freeze()) {
                                            Ok(_) => (),
                                            Err(e)=> {
                                                let _ = event_tx.send(GameNetworkEvent::Error {
                                                    connection: connection.into(),
                                                    inner: GameSocketError::SendFailed{ inner_msg: e.to_string()}
                                                });
                                                error!("Error sending packet: {:?}", e)
                                            }
                                        }
                                    }
                                }
                            }
                            BackendCommand::Shutdown => break,
                            BackendCommand::CreateStream { connection, stream, reliability } => {
                                let _ = event_tx.send(GameNetworkEvent::StreamCreated(connection.into(), GameStream::new(stream, reliability)));
                            },
                            BackendCommand::CloseStream { connection, stream } => {
                                let _ = event_tx.send(GameNetworkEvent::StreamClosed(connection.into(), stream.into()));
                            },
                        }
                    }
                }
            }
        });
    }
}

impl QuicBackend {
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
            send_streams: HashMap::new(),
        }
    }

    fn spawn_reader(conn: Connection, uuid: Uuid, event_tx: mpsc::UnboundedSender<GameNetworkEvent>) {
        let conn_clone = conn.clone();
        let event_tx_clone = event_tx.clone();

        // Datagram Reader
        tokio::spawn(async move {
            while let Ok(bytes) = conn_clone.read_datagram().await {
                if bytes.len() >= 2 {
                    let mut b = bytes;
                    let stream_id = b.get_u16();
                    let _ = event_tx_clone.send(GameNetworkEvent::Message {
                        connection: uuid.into(),
                        stream: stream_id.into(),
                        data: b,
                    });
                }
            }
        });

        // Stream Reader
        tokio::spawn(async move {
            while let Ok(mut quic_stream) = conn.accept_bi().await {
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let stream_id = match quic_stream.1.read_u16().await {
                        Ok(id) => id,
                        Err(_) => return,
                    };
                    loop {
                        let len = match quic_stream.1.read_u32().await {
                            Ok(l) => l as usize,
                            Err(_) => break,
                        };
                        let mut buf = vec![0u8; len];
                        if quic_stream.1.read_exact(&mut buf).await.is_err() { break; }

                        let _ = tx.send(GameNetworkEvent::Message {
                            connection: uuid.into(),
                            stream: stream_id.into(),
                            data: Bytes::from(buf),
                        });
                    }
                });
            }
        });
    }
}