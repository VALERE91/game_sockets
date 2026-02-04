use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use uuid::Uuid;
use quinn::{Endpoint, Connection, RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use crate::{GameConnection, GameNetworkEvent, GameSocketError, GameSocketProtocol, GameStream, GameStreamReliability};
use rustls::client::{ServerCertVerified, ServerCertVerifier};

// --- Certificate Verification Helpers ---
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

    client_config.transport_config(Arc::new(transport_config));

    client_config
}

// --- Protocol Implementation ---

pub struct QuicProtocol {
    cmd_tx: Option<mpsc::UnboundedSender<BackendCommand>>,
    event_rx: Option<mpsc::UnboundedReceiver<GameNetworkEvent>>,
    thread_handle: Option<thread::JoinHandle<()>>,
    next_stream_id: u16,
}

impl QuicProtocol {
    pub fn new() -> Self {
        Self { cmd_tx: None, event_rx: None, thread_handle: None, next_stream_id: 0 }
    }

    fn send_cmd(&self, cmd: BackendCommand) -> Result<(), GameSocketError> {
        self.cmd_tx.as_ref()
            .ok_or(GameSocketError::ConnectionError)?
            .send(cmd)
            .map_err(|_| GameSocketError::ConnectionError)
    }
}

enum BackendCommand {
    Bind { addr: String, port: u16 },
    Connect { addr: String, port: u16 },
    Send { connection: Uuid, stream: GameStream, data: Bytes },
    CreateStream { connection: GameConnection, stream: u16, reliability: GameStreamReliability },
    CloseStream { connection: GameConnection, stream: GameStream },
    Shutdown,
}

impl GameSocketProtocol for QuicProtocol {
    fn init(&mut self) -> Result<(), GameSocketError> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        self.cmd_tx = Some(cmd_tx);
        self.event_rx = Some(event_rx);

        let handle = thread::spawn(move || {
            let rt = Runtime::new().expect("Tokio runtime failed");
            rt.block_on(async move {
                let mut backend = QuicBackend::new(event_tx);
                backend.run(cmd_rx).await;
            });
        });

        self.thread_handle = Some(handle);
        Ok(())
    }

    fn listen(&mut self, _interface: &str, port: u16) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::Bind { addr: "0.0.0.0".to_string(), port })
    }

    fn connect(&mut self, remote_host: &str, remote_port: u16) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::Connect { addr: remote_host.to_string(), port: remote_port })
    }

    fn create_stream(&mut self, connection: GameConnection, reliability: GameStreamReliability) -> Result<(), GameSocketError> {
        self.next_stream_id += 1;
        self.send_cmd(BackendCommand::CreateStream { connection, stream: self.next_stream_id, reliability })
    }

    fn close_stream(&mut self, connection: GameConnection, stream: GameStream) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::CloseStream { connection, stream })
    }

    fn send(&mut self, conn: &GameConnection, stream: &GameStream, msg: Bytes) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::Send { connection: conn.connection_id, stream: stream.clone(), data: msg })
    }

    fn poll(&mut self) -> Result<Option<GameNetworkEvent>, GameSocketError> {
        match &mut self.event_rx {
            Some(rx) => match rx.try_recv() {
                Ok(e) => Ok(Some(e)),
                Err(mpsc::error::TryRecvError::Empty) => Ok(None),
                Err(_) => Err(GameSocketError::ConnectionError),
            },
            None => Err(GameSocketError::ProtocolError { inner_msg: "Not initialized".into() }),
        }
    }

    fn shutdown(&mut self) -> Result<(), GameSocketError> {
        let _ = self.send_cmd(BackendCommand::Shutdown);
        if let Some(h) = self.thread_handle.take() { let _ = h.join(); }
        Ok(())
    }
}

// --- Backend ---

struct QuicBackend {
    endpoint: Option<Endpoint>,
    connections: HashMap<Uuid, Connection>,
    send_streams: HashMap<(Uuid, u16), (SendStream, RecvStream)>,
    event_tx: mpsc::UnboundedSender<GameNetworkEvent>,
    cert_der: Option<Vec<u8>>
}

impl QuicBackend {
    fn new(event_tx: mpsc::UnboundedSender<GameNetworkEvent>) -> Self {
        Self {
            endpoint: None,
            connections: HashMap::new(),
            send_streams: HashMap::new(),
            event_tx,
            cert_der: None
        }
    }

    async fn run(&mut self, mut cmd_rx: mpsc::UnboundedReceiver<BackendCommand>) {
        let (conn_reg_tx, mut conn_reg_rx) = mpsc::unbounded_channel::<(Uuid, Connection)>();

        loop {
            tokio::select! {
                Some((uuid, conn)) = conn_reg_rx.recv() => {
                    self.connections.insert(uuid, conn);
                }

                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        BackendCommand::Bind { addr, port } => {
                            let (server_config, cert) = make_server_config();
                            self.cert_der = Some(cert);

                            let addr = format!("{}:{}", addr, port).parse().unwrap();
                            let endpoint = Endpoint::server(server_config, addr).unwrap();
                            self.endpoint = Some(endpoint.clone());

                            let event_tx = self.event_tx.clone();
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
                            let _ = self.event_tx.send(GameNetworkEvent::Connected(uuid.into()));

                            QuicBackend::spawn_reader(connection, uuid, self.event_tx.clone());
                        }
                        BackendCommand::Send { connection, stream, data } => {
                            if let Some(conn) = self.connections.get(&connection) {
                                if stream.is_reliable() {
                                    // LAZY STREAM OPENING:
                                    // If we don't have a stream for this ID, open a NEW one.
                                    // This works for Server->Client replies because the Client is listening for new streams.
                                    let key = (connection, stream.stream_id);
                                    let send_stream = if let Some(s) = self.send_streams.get_mut(&key) {
                                        s
                                    } else {
                                        let mut s = conn.open_bi().await.unwrap();
                                        let _ = s.0.write_u16(stream.stream_id).await;
                                        self.send_streams.insert(key, s);
                                        self.send_streams.get_mut(&key).unwrap()
                                    };

                                    let mut frame = BytesMut::with_capacity(4 + data.len());
                                    frame.put_u32(data.len() as u32);
                                    frame.put(data);
                                    let _ = send_stream.0.write_all(&frame).await;
                                } else {
                                    let mut packet = BytesMut::with_capacity(2 + data.len());
                                    packet.put_u16(stream.stream_id);
                                    packet.put(data);
                                    let _ = conn.send_datagram(packet.freeze());
                                }
                            }
                        }
                        BackendCommand::Shutdown => break,
                        BackendCommand::CreateStream { connection, stream, reliability } => {
                            let _ = self.event_tx.send(GameNetworkEvent::StreamCreated(connection, GameStream::new(stream, reliability)));
                        },
                        BackendCommand::CloseStream { connection, stream } => {
                            let _ = self.event_tx.send(GameNetworkEvent::StreamClosed(connection, stream.into()));
                        },
                    }
                }
            }
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