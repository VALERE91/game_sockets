use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use uuid::Uuid;
use crate::{GameConnection, GameNetworkEvent, GameSocketError, GameSocketProtocol, GameStream, GameStreamReliability};

pub struct TcpProtocol {
    cmd_tx: Option<mpsc::UnboundedSender<BackendCommand>>,
    event_rx: Option<mpsc::UnboundedReceiver<GameNetworkEvent>>,
    thread_handle: Option<thread::JoinHandle<()>>,
    next_stream_id: u16,
}

impl TcpProtocol {
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
    Send { connection: Uuid, stream: u16, data: Bytes },
    CreateStream { connection: uuid::Uuid, stream: u16, reliability: GameStreamReliability },
    CloseStream { connection: uuid::Uuid, stream: u16 },
    Shutdown,
}

impl GameSocketProtocol for TcpProtocol {
    fn init(&mut self) -> Result<(), GameSocketError> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        self.cmd_tx = Some(cmd_tx);
        self.event_rx = Some(event_rx);

        let handle = thread::spawn(move || {
            let rt = Runtime::new().expect("Failed to create Tokio runtime");
            rt.block_on(async move {
                let mut backend = TcpBackend::new(event_tx);
                backend.run(cmd_rx).await;
            });
        });

        self.thread_handle = Some(handle);
        Ok(())
    }

    fn listen(&mut self, interface: &str, port: u16) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::Bind { addr: interface.to_string(), port })
    }

    fn connect(&mut self, remote_host: &str, remote_port: u16) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::Connect { addr: remote_host.to_string(), port: remote_port })
    }

    fn create_stream(&mut self, connection: GameConnection, reliability: GameStreamReliability) -> Result<(), GameSocketError> {
        self.next_stream_id += 1;
        self.send_cmd(BackendCommand::CreateStream {
            connection: connection.connection_id,
            stream: self.next_stream_id,
            reliability: reliability
        })
    }

    fn close_stream(&mut self, conn: GameConnection, stream: GameStream) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::CloseStream { connection: conn.connection_id, stream: stream.stream_id })
    }

    fn send(&mut self, conn: &GameConnection, stream: &GameStream, msg: Bytes) -> Result<(), GameSocketError> {
        self.send_cmd(BackendCommand::Send { connection: conn.connection_id, stream: stream.stream_id, data: msg })
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

struct TcpBackend {
    // Map UUID -> Transmit Channel for that specific TCP connection
    peers: HashMap<Uuid, mpsc::Sender<Bytes>>,
    event_tx: mpsc::UnboundedSender<GameNetworkEvent>,
}

impl TcpBackend {
    fn new(event_tx: mpsc::UnboundedSender<GameNetworkEvent>) -> Self {
        Self { peers: HashMap::new(), event_tx }
    }

    async fn run(&mut self, mut cmd_rx: mpsc::UnboundedReceiver<BackendCommand>) {
        // Channel for the Accept Task to register new peers back to us
        let (peer_reg_tx, mut peer_reg_rx) = mpsc::channel::<(Uuid, mpsc::Sender<Bytes>)>(16);

        loop {
            tokio::select! {
                // New Peer Registered (from Connect or Accept)
                Some((uuid, tx)) = peer_reg_rx.recv() => {
                    self.peers.insert(uuid, tx);
                }

                // Commands from Game Loop
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        BackendCommand::Bind { addr, port } => {
                            let listener = TcpListener::bind(format!("{}:{}", addr, port)).await.expect("Bind failed");
                            let event_tx = self.event_tx.clone();
                            let peer_reg_tx = peer_reg_tx.clone();

                            // Spawn Listener Task
                            tokio::spawn(async move {
                                while let Ok((socket, _)) = listener.accept().await {
                                    let uuid = Uuid::new_v4();
                                    // Notify Game Thread
                                    let _ = event_tx.send(GameNetworkEvent::Connected(uuid.into()));

                                    // Setup Peer Handler
                                    let (write_tx, write_rx) = mpsc::channel(100);
                                    TcpBackend::spawn_peer_handler(socket, uuid, event_tx.clone(), write_rx);

                                    // Register Write Handle
                                    let _ = peer_reg_tx.send((uuid, write_tx)).await;
                                }
                            });
                        }
                        BackendCommand::Connect { addr, port } => {
                            if let Ok(socket) = TcpStream::connect(format!("{}:{}", addr, port)).await {
                                let uuid = Uuid::new_v4();
                                let _ = self.event_tx.send(GameNetworkEvent::Connected(uuid.into()));

                                let (write_tx, write_rx) = mpsc::channel(100);
                                TcpBackend::spawn_peer_handler(socket, uuid, self.event_tx.clone(), write_rx);

                                self.peers.insert(uuid, write_tx);
                            } else {
                                let _ = self.event_tx.send(GameNetworkEvent::Error {
                                    connection: Default::default(),
                                    inner: GameSocketError::ConnectionError,
                                });
                            }
                        }
                        BackendCommand::Send { connection, stream, data } => {
                            if let Some(tx) = self.peers.get(&connection) {
                                // Prepend StreamID to payload
                                let mut packet = BytesMut::with_capacity(2 + data.len());
                                packet.put_u16(stream);
                                packet.put(data);

                                // Send to Handler (which frames it with Length)
                                let _ = tx.send(packet.freeze()).await;
                            }
                        }
                        BackendCommand::Shutdown => break,
                        BackendCommand::CreateStream{ connection, stream, reliability } => {
                            let _ = self.event_tx.send(GameNetworkEvent::StreamCreated(connection.into(), GameStream::new(stream, reliability)));
                        }
                        BackendCommand::CloseStream{ connection, stream } => {
                            let _ = self.event_tx.send(GameNetworkEvent::StreamClosed(connection.into(), stream.into()));
                        }
                    }
                }
            }
        }
    }

    /// Spawns a task that manages Reading AND Writing for a single socket.
    fn spawn_peer_handler(
        socket: TcpStream,
        uuid: Uuid,
        event_tx: mpsc::UnboundedSender<GameNetworkEvent>,
        mut write_rx: mpsc::Receiver<Bytes>
    ) {
        tokio::spawn(async move {
            // Apply Length-Delimited Framing
            // This transforms the raw stream into "packets" based on a u32 length header.
            let mut framed = Framed::new(socket, LengthDelimitedCodec::new());

            loop {
                tokio::select! {
                    // READ: Socket -> Game
                    maybe_frame = framed.next() => {
                        match maybe_frame {
                            Some(Ok(mut bytes)) => {
                                // Protocol: [StreamID: u16] [Payload...]
                                if bytes.len() >= 2 {
                                    let stream_id = bytes.get_u16(); // Consumes first 2 bytes
                                    let payload = bytes.freeze();

                                    let _ = event_tx.send(GameNetworkEvent::Message {
                                        connection: uuid.into(),
                                        stream: stream_id.into(),
                                        data: payload,
                                    });
                                }
                            }
                            Some(Err(_)) | None => {
                                let _ = event_tx.send(GameNetworkEvent::Disconnected(uuid.into()));
                                break;
                            }
                        }
                    }

                    // WRITE: Game -> Socket
                    Some(packet) = write_rx.recv() => {
                        // The Codec automatically prepends the Length Header
                        let _ = framed.send(packet).await;
                    }
                }
            }
        });
    }
}