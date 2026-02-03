pub mod protocols;

use thiserror::Error;

pub trait GameSocketProtocol {
    fn init(&mut self) -> Result<(), GameSocketError>;
    fn listen(&mut self, interface: &str, port: u16) -> Result<(), GameSocketError>;
    fn connect(&mut self, remote_host: &str, remote_port: u16) -> Result<(), GameSocketError>;
    fn create_stream(&mut self, conn: GameConnection, reliability: GameStreamReliability) -> Result<(), GameSocketError>;
    fn close_stream(&mut self, conn: GameConnection, stream: GameStream) -> Result<(), GameSocketError>;
    fn send(&mut self, conn: &GameConnection, stream: &GameStream, msg: bytes::Bytes) -> Result<(), GameSocketError>;
    fn poll(&mut self) -> Result<Option<GameNetworkEvent>, GameSocketError>;
    fn shutdown(&mut self) -> Result<(), GameSocketError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GamePeer<TProtocol: GameSocketProtocol> {
    pub protocol: TProtocol
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Hash)]
pub struct GameConnection {
    pub connection_id: uuid::Uuid
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct GameStream {
    pub stream_id: u16
}

const RELIABILITY_MASK: u16 = 0b11;
const ORDERING_MASK: u16 = 0b10;

impl GameStream {
    pub fn new(stream_id: u16, game_stream_reliability: GameStreamReliability) -> Self {
        let mut stream_id = stream_id << 2;
        if game_stream_reliability == GameStreamReliability::Ordered {
            stream_id |= ORDERING_MASK;
        }
        if game_stream_reliability == GameStreamReliability::Reliable {
            stream_id |= RELIABILITY_MASK;
        }

        Self {
            stream_id
        }
    }

    pub fn is_reliable(&self) -> bool {
        //Check last bit of stream_id
        self.stream_id & RELIABILITY_MASK != 0
    }

    pub fn is_ordered(&self) -> bool {
        //Check second last bit of stream_id
        self.stream_id & ORDERING_MASK != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GameStreamReliability {
    Reliable,
    Unreliable,
    Ordered,
}

#[derive(Debug, Error)]
pub enum GameSocketError {
    #[error("Generic error from protocol : {inner_msg}.")]
    ProtocolError { inner_msg: String },
    #[error("Error initializing protocol : {inner_msg}.")]
    InitError { inner_msg: String},
    #[error("Error connecting to remote host.")]
    ConnectionError,
    #[error("Unable to bind socket.")]
    BindError(#[from] std::io::Error),
}

#[derive(Debug)]
pub enum GameNetworkEvent {
    Connected(GameConnection),
    Disconnected(GameConnection),
    Message{
        connection: GameConnection,
        stream: GameStream,
        data: bytes::Bytes
    },
    Error {
        connection: GameConnection,
        inner: GameSocketError
    },
    StreamCreated(GameStream),
    StreamClosed(GameStream),
}

impl<P: GameSocketProtocol> GamePeer<P> {
    pub fn new(protocol: P) -> Self {
        Self { protocol }
    }

    pub fn listen(&mut self, port: u16) -> Result<(), GameSocketError> {
        self.protocol.init()?;
        self.protocol.listen("0.0.0.0", port)
    }

    pub fn connect(&mut self, addr: &str, port: u16) -> Result<(), GameSocketError> {
        self.protocol.init()?;
        self.protocol.connect(addr, port)
    }

    pub fn create_stream(&mut self, conn: GameConnection, reliability: GameStreamReliability) -> Result<(), GameSocketError> {
        self.protocol.create_stream(conn, reliability)
    }

    pub fn close_stream(&mut self, conn: GameConnection, stream: GameStream) -> Result<(), GameSocketError> {
        self.protocol.close_stream(conn, stream)
    }

    pub fn send(&mut self, conn: &GameConnection, stream: &GameStream, msg: bytes::Bytes) {
        let _ = self.protocol.send(conn, stream, msg);
    }

    pub fn poll(&mut self) -> Result<Option<GameNetworkEvent>, GameSocketError> {
        self.protocol.poll()
    }
}

impl From<uuid::Uuid> for GameConnection {
    fn from(id: uuid::Uuid) -> Self {
        Self { connection_id: id }
    }
}

impl From<u16> for GameStream {
    fn from(id: u16) -> Self {
        Self { stream_id: id }
    }
}