mod utils;

use std::vec;
use tracing::{debug, error, info, warn};
use game_sockets::*;
use game_sockets::protocols::*;

struct GlobalState {
    clients: Vec<GameConnection>,
}

impl GlobalState {
    pub fn new() -> Self {
        Self {
            clients: vec![],
        }
    }

    pub fn add_client(&mut self, connection: GameConnection) {
        self.clients.push(connection);
    }

    pub fn remove_client(&mut self, connection: GameConnection) {
        self.clients.retain(|c| c != &connection);
    }
}

fn main() -> Result<(), GameSocketError>{
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let protocol = UdpProtocol::new();
    let mut server = GamePeer::new(protocol);
    server.listen(8080)?;
    info!("Server started on port 8080");
    let mut state = GlobalState::new();

    //Read all the server events
    loop {
        let event = server.poll();
        let Ok(event) = event
        else {
            // Socket Error
            error!("Error polling server");
            return Ok(());
        };
        let Some(event) = event else {
            //No more events
            continue;
        };

        match event {
            GameNetworkEvent::Connected(connection) => {
                info!("Client connected: {:?}", connection);
                state.add_client(connection);
            },
            GameNetworkEvent::Disconnected(connection) => {
                info!("Client disconnected: {:?}", connection);
                state.remove_client(connection);
            },
            GameNetworkEvent::Message {connection, stream, data } => {
                use utils::BenchmarkPacket;
                let Some(packet) = BenchmarkPacket::from_bytes(data) else {
                    warn!("Received invalid packet from client: {:?}", connection);
                    continue;
                };
                debug!("Received packet {} from client: {:?}", packet.id, connection);
                server.send(&connection, &stream, packet.to_bytes())
            },
            _ => {}
        }
    }
}