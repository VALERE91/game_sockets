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

#[derive(Debug)]
pub struct UdpProtocol {
    /// Channel to send commands to the background runtime
    cmd_tx: Option<mpsc::UnboundedSender<BackendCommand>>,

    /// Channel to receive events from the background runtime
    event_rx: Option<mpsc::UnboundedReceiver<GameNetworkEvent>>,

    /// Handle to the background thread (so we can join it on drop if needed)
    thread_handle: Option<thread::JoinHandle<()>>,

    /// Next Stream ID to use for new streams
    next_stream_id: u16,
}

impl UdpProtocol {
    pub fn new() -> Self {
        Self {
            cmd_tx: None,
            event_rx: None,
            thread_handle: None,
            next_stream_id: 0,
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
    CreateStream { connection: uuid::Uuid, stream: u16, reliability: GameStreamReliability },
    CloseStream { connection: uuid::Uuid, stream: u16 },
    Shutdown,
}

impl GameSocketProtocol for UdpProtocol {
    fn init(&mut self) -> Result<(), GameSocketError> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<BackendCommand>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<GameNetworkEvent>();

        self.cmd_tx = Some(cmd_tx);
        self.event_rx = Some(event_rx);

        // Spawn thread, but delegate logic to the Backend struct
        let handle = thread::spawn(move || {
            let rt = Runtime::new().expect("Failed to create Tokio runtime");
            let mut backend = UdpBackend::new(event_tx);
            rt.block_on(backend.run(cmd_rx));
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

    fn create_stream(&mut self, conn: GameConnection, reliability: GameStreamReliability) -> Result<(), GameSocketError> {
        self.next_stream_id += 1;
        self.send_cmd(BackendCommand::CreateStream {
            connection: conn.connection_id,
            stream: self.next_stream_id,
            reliability: reliability
        })
    }

    fn close_stream(&mut self, _conn: GameConnection, _stream: GameStream) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::CloseStream { connection: _conn.connection_id, stream: _stream.stream_id })
    }

    fn send(&mut self, conn: &GameConnection, stream: &GameStream, msg: Bytes) -> Result<(), GameSocketError> {
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

struct UdpBackend {
    socket: Option<Arc<UdpSocket>>,
    connections: std::collections::HashMap<Uuid, SocketAddr>,
    event_tx: mpsc::UnboundedSender<GameNetworkEvent>,
}

impl UdpBackend {
    fn new(event_tx: mpsc::UnboundedSender<GameNetworkEvent>) -> Self {
        Self {
            socket: None,
            connections: std::collections::HashMap::new(),
            event_tx,
        }
    }

    /// Main Event Loop
    async fn run(&mut self, mut cmd_rx: mpsc::UnboundedReceiver<BackendCommand>) {
        let mut buf = [0u8; 2048];

        loop {
            // We construct the receive future here to satisfy the borrow checker
            let recv_future = async {
                match &self.socket {
                    Some(s) => s.recv_from(&mut buf).await,
                    None => std::future::pending().await,
                }
            };

            tokio::select! {
                //Handle Commands
                Some(cmd) = cmd_rx.recv() => {
                    if matches!(cmd, BackendCommand::Shutdown) { break; }
                    self.process_command(cmd).await;
                }

                //Handle Network Traffic
                res = recv_future => {
                    match res {
                        Ok((len, addr)) => self.process_packet(&buf[..len], addr),
                        Err(_) => tokio::task::yield_now().await, // Simple error backoff
                    }
                }
            }
        }
    }

    /// Command Processor
    async fn process_command(&mut self, cmd: BackendCommand) {
        match cmd {
            BackendCommand::Bind { interface, port } => {
                // Only bind if we don't have a socket
                if self.socket.is_none() {
                    if let Ok(s) = UdpSocket::bind(format!("{}:{}", interface, port)).await {
                        self.socket = Some(Arc::new(s));
                    }
                }
            }
            BackendCommand::Connect { addr, port } => {
                if let Ok(socket_addr) = format!("{}:{}", addr, port).parse::<SocketAddr>() {
                    let uuid = Uuid::new_v4();
                    self.connections.insert(uuid, socket_addr);
                    let _ = self.event_tx.send(GameNetworkEvent::Connected(uuid.into()));
                }
            }
            BackendCommand::Send { connection, stream, data } => {
                if let Some(socket) = &self.socket {
                    if let Some(remote_addr) = self.connections.get(&connection) {
                        let mut packet = BytesMut::with_capacity(HEADER_SIZE + data.len());
                        packet.put_slice(connection.as_bytes());
                        packet.put_u16(stream);
                        packet.put(data);
                        let _ = socket.send_to(&packet, remote_addr).await;
                    }
                }
            }
            BackendCommand::CreateStream { connection: _, stream, reliability } => {
                let _ = self.event_tx.send(GameNetworkEvent::StreamCreated(GameStream::new(stream, reliability)));
            }
            BackendCommand::CloseStream { connection: _, stream } => {
                let _ = self.event_tx.send(GameNetworkEvent::StreamClosed(stream.into()));
            }
            _ => {} // Handled in loop
        }
    }

    /// Packet Processor
    fn process_packet(&mut self, buf: &[u8], addr: SocketAddr) {
        if buf.len() < HEADER_SIZE { return; }

        let Ok(incoming_uuid) = Uuid::from_slice(&buf[0..16]) else { return };
        let stream_id = u16::from_be_bytes([buf[16], buf[17]]);

        // Auto-Accept / Update Address Logic
        match self.connections.entry(incoming_uuid) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(addr);
                let _ = self.event_tx.send(GameNetworkEvent::Connected(incoming_uuid.into()));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if *entry.get() != addr {
                    entry.insert(addr);
                }
            }
        }

        // Dispatch Message
        let payload = Bytes::copy_from_slice(&buf[HEADER_SIZE..]);
        let _ = self.event_tx.send(GameNetworkEvent::Message {
            connection: incoming_uuid.into(),
            stream: stream_id.into(),
            data: payload
        });
    }
}