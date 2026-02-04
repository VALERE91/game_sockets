use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4};
use std::str::FromStr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

// Use the crate as requested
use gns::sys::*;
use gns::*;

use crate::{GameConnection, GameNetworkEvent, GameSocketError, GameSocketProtocol, GameStream, GameStreamReliability};

// --- Backend Commands ---
enum BackendCommand {
    Bind { port: u16 },
    Connect { addr: String, port: u16 },
    Send { connection: Uuid, stream: GameStream, data: Bytes },
    Shutdown,
}

enum ActiveSocket {
    Server(GnsSocket<IsServer>),
    Client(GnsSocket<IsClient>),
}

impl ActiveSocket {
    /// Generic send wrapper
    fn send_messages(&self, messages: Vec<GnsNetworkMessage<ToSend>>) {
        match self {
            ActiveSocket::Server(s) => s.send_messages(messages),
            ActiveSocket::Client(c) => c.send_messages(messages),
        };
    }

    /// Generic event poll wrapper
    fn poll_event<F>(&self, callback: F) where F: FnMut(GnsConnectionEvent)
    {
        match self {
            ActiveSocket::Server(s) => { let _ = s.poll_event::<100>(callback); },
            ActiveSocket::Client(c) => { let _ = c.poll_event::<100>(callback); },
        }
    }

    /// Generic message poll wrapper
    fn poll_messages<F>(&self, callback: F) where F: FnMut(&GnsNetworkMessage<ToReceive>)
    {
        match self {
            ActiveSocket::Server(s) => { let _ = s.poll_messages::<100>(callback); },
            ActiveSocket::Client(c) => { let _ = c.poll_messages::<100>(callback); },
        }
    }

    /// Helper to close connection on either socket type
    fn close_connection(&self, conn: GnsConnection) {
        match self {
            ActiveSocket::Server(s) => s.close_connection(conn, 0, "", false),
            ActiveSocket::Client(c) => c.close_connection(conn, 0, "", false),
        };
    }

    /// Helper to accept connections (No-op if client)
    fn try_accept(&self, conn: GnsConnection) -> Result<(), EResult> {
        if let ActiveSocket::Server(s) = self {
            s.accept(conn)
        } else {
            Ok(()) // Clients auto-accept implicitly by initiating, or ignore
        }
    }
}

// --- Protocol Implementation ---

pub struct GnsProtocol {
    cmd_tx: Option<mpsc::UnboundedSender<BackendCommand>>,
    event_rx: Option<mpsc::UnboundedReceiver<GameNetworkEvent>>,
    thread_handle: Option<thread::JoinHandle<()>>,
    next_stream_id: u16,
}

impl GnsProtocol {
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

impl GameSocketProtocol for GnsProtocol {
    fn init(&mut self) -> Result<(), GameSocketError> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        self.cmd_tx = Some(cmd_tx);
        self.event_rx = Some(event_rx);

        let handle = thread::spawn(move || {
            // We use a dedicated thread for the GNS loop
            let mut backend = GnsBackend::new(event_tx);
            backend.run(cmd_rx);
        });

        self.thread_handle = Some(handle);
        Ok(())
    }

    fn listen(&mut self, _interface: &str, port: u16) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::Bind { port })
    }

    fn connect(&mut self, remote_host: &str, remote_port: u16) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::Connect { addr: remote_host.to_string(), port: remote_port })
    }

    fn create_stream(&mut self, _connection: GameConnection, _reliability: GameStreamReliability) -> Result<(), GameSocketError> {
        // GNS doesn't need explicit stream creation, we just track IDs
        self.next_stream_id += 1;
        Ok(())
    }

    fn close_stream(&mut self, _: GameConnection, _: GameStream) -> Result<(), GameSocketError> { Ok(()) }

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

struct GnsBackend {
    gns_global: Option<Arc<GnsGlobal>>,
    // Unified Socket Container
    socket: Option<ActiveSocket>,

    handle_to_uuid: HashMap<GnsConnection, Uuid>,
    uuid_to_handle: HashMap<Uuid, GnsConnection>,

    event_tx: mpsc::UnboundedSender<GameNetworkEvent>,
}

impl GnsBackend {
    fn new(event_tx: mpsc::UnboundedSender<GameNetworkEvent>) -> Self {
        Self {
            gns_global: None,
            socket: None,
            handle_to_uuid: HashMap::new(),
            uuid_to_handle: HashMap::new(),
            event_tx,
        }
    }

    fn run(&mut self, mut cmd_rx: mpsc::UnboundedReceiver<BackendCommand>) {
        // 1. Initialize GNS Global
        let gns_global = match GnsGlobal::get() {
            Ok(g) => g,
            Err(_) => {
                error!("Failed to initialize GNS Global. Aborting backend.");
                return;
            }
        };

        // Enable Debug Output
        gns_global.utils().enable_debug_output(
            ESteamNetworkingSocketsDebugOutputType::k_ESteamNetworkingSocketsDebugOutputType_Msg,
            |ty, message| debug!("[GNS Debug] {:#?}: {}", ty, message),
        );

        self.gns_global = Some(gns_global.clone());

        loop {
            // A. Process Commands
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    BackendCommand::Bind { port } => {
                        let socket = GnsSocket::new(gns_global.clone());

                        match socket.listen(Ipv4Addr::UNSPECIFIED.into(), port) {
                            Ok(s) => {
                                info!("GNS Listening on port {}", port);
                                self.socket = Some(ActiveSocket::Server(s));
                            }
                            Err(e) => error!("GNS Listen Failed: {:?}", e),
                        }
                    }
                    BackendCommand::Connect { addr, port } => {
                        let socket = GnsSocket::new(gns_global.clone());
                        let ip = Ipv4Addr::from_str(&addr).unwrap_or(Ipv4Addr::LOCALHOST);

                        match socket.connect(ip.into(), port) {
                            Ok(s) => {
                                info!("GNS Connecting to {}:{}", addr, port);
                                self.socket = Some(ActiveSocket::Client(s));
                            }
                            Err(e) => error!("GNS Connect Failed: {:?}", e),
                        }
                    }
                    BackendCommand::Send { connection, stream, data } => {
                        if let Some(conn_handle) = self.uuid_to_handle.get(&connection) {
                            if let Some(socket) = &self.socket {
                                let flags = if stream.is_reliable() {
                                    k_nSteamNetworkingSend_Reliable
                                } else {
                                    k_nSteamNetworkingSend_Unreliable
                                };

                                // Framing
                                let mut packet_buf = BytesMut::with_capacity(2 + data.len());
                                packet_buf.put_u16(stream.stream_id);
                                packet_buf.put(data);
                                let final_bytes = packet_buf.freeze();

                                // Allocation
                                let msg = gns_global.utils().allocate_message(
                                    *conn_handle,
                                    flags,
                                    &final_bytes,
                                );

                                // Dispatch
                                socket.send_messages(vec![msg]);
                            }
                        }
                    }
                    BackendCommand::Shutdown => return,
                }
            }

            // B. Poll Callbacks
            gns_global.poll_callbacks();

            // C. Poll Socket (Unified)
            if let Some(socket) = self.socket.take() {

                // 1. Poll Events
                socket.poll_event(|event| {
                    self.handle_event(&socket, event);
                });

                // 2. Poll Messages
                socket.poll_messages(|message| {
                    self.handle_message(message);
                });

                // Put the socket back
                self.socket = Some(socket);
            }

            // D. Yield
            thread::sleep(Duration::from_millis(1));
        }
    }

    // --- Event Handlers ---

    // Fixed: Takes event by value (GnsConnectionEvent)
    fn handle_event(&mut self, socket: &ActiveSocket, event: GnsConnectionEvent) {
        let old_state = event.old_state();
        let new_state = event.info().state();
        let conn = event.connection();

        match (old_state, new_state) {
            // Accept Connection (Server Only)
            (ESteamNetworkingConnectionState::k_ESteamNetworkingConnectionState_None,
                ESteamNetworkingConnectionState::k_ESteamNetworkingConnectionState_Connecting) => {

                if let Err(e) = socket.try_accept(conn) {
                    error!("Failed to accept client: {:?}", e);
                } else {
                    let uuid = Uuid::new_v4();
                    self.handle_to_uuid.insert(conn, uuid);
                    self.uuid_to_handle.insert(uuid, conn);
                    let _ = self.event_tx.send(GameNetworkEvent::Connected(uuid.into()));
                    info!("GNS Accepted Client: {:?}", uuid);
                }
            }

            // Connected (Client or Server completion)
            (ESteamNetworkingConnectionState::k_ESteamNetworkingConnectionState_Connecting,
                ESteamNetworkingConnectionState::k_ESteamNetworkingConnectionState_Connected) => {
                // Register if not already known
                if !self.handle_to_uuid.contains_key(&conn) {
                    let uuid = Uuid::new_v4();
                    self.handle_to_uuid.insert(conn, uuid);
                    self.uuid_to_handle.insert(uuid, conn);
                    let _ = self.event_tx.send(GameNetworkEvent::Connected(uuid.into()));
                    info!("GNS Connection Established: {:?}", uuid);
                }
            }

            // Disconnect
            (_, ESteamNetworkingConnectionState::k_ESteamNetworkingConnectionState_ClosedByPeer) |
            (_, ESteamNetworkingConnectionState::k_ESteamNetworkingConnectionState_ProblemDetectedLocally) => {
                if let Some(uuid) = self.handle_to_uuid.remove(&conn) {
                    self.uuid_to_handle.remove(&uuid);
                    let _ = self.event_tx.send(GameNetworkEvent::Disconnected(uuid.into()));
                    info!("GNS Disconnected: {:?}", uuid);
                }
                socket.close_connection(conn);
            }
            _ => {}
        }
    }

    // Fixed: Takes message by value (GnsMessage)
    fn handle_message(&mut self, message: &GnsNetworkMessage<ToReceive>) {
        let payload = message.payload();
        let conn = message.connection();

        if let Some(uuid) = self.handle_to_uuid.get(&conn) {
            if payload.len() >= 2 {
                // De-Frame
                let stream_id = u16::from_be_bytes([payload[0], payload[1]]);
                let data = Bytes::copy_from_slice(&payload[2..]);

                let _ = self.event_tx.send(GameNetworkEvent::Message {
                    connection: (*uuid).into(),
                    stream: stream_id.into(),
                    data,
                });
            }
        }
    }
}