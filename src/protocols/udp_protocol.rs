use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;
use bytes::{BufMut, Bytes, BytesMut};
use tokio::net::UdpSocket;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use uuid::Uuid;
use crate::{GameConnection, GameNetworkEvent, GameSocketError, GameSocketProtocol, GameStream, GameStreamReliability};

// Protocol Constants
const HEADER_SIZE: usize = 18; // 16 bytes (UUID) + 2 bytes (StreamID)

pub struct UdpProtocol {
    /// Channel to send commands to the background runtime
    cmd_tx: Option<mpsc::UnboundedSender<BackendCommand>>,
    cmd_rx: Option<mpsc::UnboundedReceiver<BackendCommand>>,

    /// Channel to receive events from the background runtime
    event_rx: Option<mpsc::UnboundedReceiver<GameNetworkEvent>>,
    event_tx: Option<mpsc::UnboundedSender<GameNetworkEvent>>,

    /// Handle to the background thread (so we can join it on drop if needed)
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl UdpProtocol {
    pub fn new() -> Self {
        Self {
            cmd_tx: None,
            cmd_rx: None,
            event_rx: None,
            event_tx: None,
            thread_handle: None,
        }
    }

    // Helper to reduce boilerplate
    fn send_cmd(&self, cmd: BackendCommand) -> Result<(), GameSocketError> {
        self.cmd_tx.as_ref()
            .ok_or(GameSocketError::ConnectionError)?
            .send(cmd)
            .map_err(|_| GameSocketError::ConnectionError)
    }
}

/// Commands sent from the Game Thread to the Async Runtime
enum BackendCommand {
    Bind { interface: String, port: u16 },
    Connect { addr: String, port: u16 },
    Send { connection: uuid::Uuid, stream: u16, data: Bytes },
    CreateStream { connection: uuid::Uuid, reliability: GameStreamReliability },
    CloseStream { connection: uuid::Uuid, stream: u16 },
    Shutdown,
}

impl GameSocketProtocol for UdpProtocol {
    fn init(&mut self) -> Result<(), GameSocketError> {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<BackendCommand>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<GameNetworkEvent>();

        self.cmd_tx = Some(cmd_tx);
        self.event_rx = Some(event_rx);

        // Spawn the Tokio Runtime on a dedicated thread
        let handle = thread::spawn(move || {
            let rt = Runtime::new().expect("Failed to create Tokio runtime");

            rt.block_on(async move {
                // State held by the async backend
                let mut socket: Option<Arc<UdpSocket>> = None;
                // Simple map to associate UUIDs with Addresses (Virtual Connection)
                let mut connections: std::collections::HashMap<uuid::Uuid, SocketAddr> = std::collections::HashMap::new();

                // Buffer for reading
                let mut buf = [0u8; 1400]; // Standard MTU safe size

                loop {
                    // We need to select between reading from the socket (if it exists)
                    // and reading commands from the game thread.
                    tokio::select! {
                        // 1. Handle Commands from Game Thread
                        Some(cmd) = cmd_rx.recv() => {
                            match cmd {
                                BackendCommand::Bind { interface, port } => {
                                    match UdpSocket::bind(format!("{}:{}", interface, port)).await {
                                        Ok(s) => socket = Some(Arc::new(s)),
                                        Err(_) => {
                                            return
                                        }
                                    }
                                }
                                BackendCommand::Connect { addr, port } => {
                                    // In UDP, "Connect" just means remembering the address
                                    let uuid = uuid::Uuid::new_v4();
                                    let addr_str = format!("{}:{}", addr, port);
                                    let addr = addr_str.parse::<SocketAddr>()
                                            .map_err(|_| GameSocketError::ConnectionError);
                                    let Ok(addr) = addr else {
                                        return
                                    };
                                    connections.insert(uuid, addr);
                                    let _ = event_tx.send(GameNetworkEvent::Connected(uuid.into()));
                                }
                                BackendCommand::Send { connection, stream, data } => {
                                    if let Some(socket) = &socket {
                                        if let Some(remote_addr) = connections.get(&connection) {
                                            // Capacity = 16 (UUID) + 2 (Stream) + Data Len
                                            let mut packet = BytesMut::with_capacity(HEADER_SIZE + data.len());

                                            // Write Header
                                            packet.put_slice(connection.as_bytes());
                                            packet.put_u16(stream); // Network Endian (Big Endian) by default

                                            // Write Payload
                                            packet.put(data);

                                            // Send
                                            let _ = socket.send_to(&packet, remote_addr).await;
                                        }
                                    }
                                }
                                BackendCommand::Shutdown => break,
                                BackendCommand::CreateStream{ .. } => {},
                                BackendCommand::CloseStream{ .. } => {}}
                        }

                        // 2. Handle Incoming UDP Packets
                        // We only poll this branch if the socket is actually bound
                        Ok((len, addr)) = async {
                            match &socket {
                                Some(s) => s.recv_from(&mut buf).await,
                                None => std::future::pending().await,
                            }
                        } => {
                            if len < HEADER_SIZE {
                                continue;
                            }

                            // UUID is bytes 0..16
                            // StreamID is bytes 16..18
                            let uuid_bytes = &buf[0..16];
                            let Ok(incoming_uuid) = Uuid::from_slice(uuid_bytes) else {continue;};
                            let stream_id = u16::from_be_bytes([buf[16], buf[17]]);

                            // If we don't know this UUID, it's a new client.
                            match connections.entry(incoming_uuid) {
                                std::collections::hash_map::Entry::Vacant(entry) => {
                                    entry.insert(addr);
                                    let _ = event_tx.send(GameNetworkEvent::Connected(incoming_uuid.into()));
                                }
                                std::collections::hash_map::Entry::Occupied(mut entry) => {
                                    // Handle NAT rebinding (client IP changed but UUID is same)
                                    if *entry.get() != addr {
                                        entry.insert(addr);
                                    }
                                }
                            }

                            // Data starts at offset 18
                            let payload = Bytes::copy_from_slice(&buf[HEADER_SIZE..len]);

                            let _ = event_tx.send(GameNetworkEvent::Message {
                                connection: incoming_uuid.into(),
                                stream: stream_id.into(),
                                data: payload
                            });
                        }
                    }
                }
            });
        });

        self.thread_handle = Some(handle);
        Ok(())
    }

    fn listen(&mut self, interface: &str, port: u16) -> Result<(), GameSocketError> {
        // Send command to bind
        self.send_cmd(BackendCommand::Bind { interface: interface.to_string(), port })
    }

    fn connect(&mut self, remote_host: &str, remote_port: u16) -> Result<(), GameSocketError> {
        // Send command to connect
        self.send_cmd(BackendCommand::Bind { interface: "0.0.0.0".to_string(), port: 0 })?;
        self.send_cmd(BackendCommand::Connect { addr: remote_host.to_string(), port: remote_port })
    }

    fn create_stream(&mut self, _conn: GameConnection, _reliability: GameStreamReliability) -> Result<(), GameSocketError> {
        // Since stream creation is not supported (automatic) in UDP, we just ignore it.
        Ok(())
    }

    fn close_stream(&mut self, _conn: GameConnection, _stream: GameStream) -> Result<(), GameSocketError> {
        // Since stream destruction is not supported (automatic) in UDP, we just ignore it.
        Ok(())
    }

    fn send(&mut self, conn: GameConnection, stream: GameStream, msg: Bytes) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::Bind { interface: "0.0.0.0".to_string(), port: 0})?;
        self.send_cmd(BackendCommand::Send { connection: conn.connection_id, stream: stream.stream_id, data: msg })
    }

    fn poll(&mut self) -> Result<Option<GameNetworkEvent>, GameSocketError> {
        if let Some(rx) = &mut self.event_rx {
            match rx.try_recv() {
                Ok(event) => Ok(Some(event)),
                Err(mpsc::error::TryRecvError::Empty) => Ok(None),
                Err(mpsc::error::TryRecvError::Disconnected) => Err(GameSocketError::ConnectionError),
            }
        } else {
            Err(GameSocketError::ProtocolError { inner_msg: "Protocol not initialized".to_string()})
        }
    }

    fn shutdown(&mut self) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::Shutdown)?;
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
        Ok(())
    }
}