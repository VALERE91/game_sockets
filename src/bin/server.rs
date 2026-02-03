mod utils;

use std::vec;
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
    let protocol = UdpProtocol::new();
    let mut server = GamePeer::new(protocol);
    server.listen(8080)?;
    println!("Server started on port 8080");
    let mut state = GlobalState::new();

    //Read all the server events
    loop {
        let event = server.poll();
        let Ok(event) = event
        else {
            // Socket Error
            return Ok(());
        };
        let Some(event) = event else {
            //No more events
            continue;
        };

        match event {
            GameNetworkEvent::Connected(connection) => {
                println!("Client connected: {:?}", connection);
                state.add_client(connection);
            },
            GameNetworkEvent::Disconnected(connection) => {
                println!("Client disconnected: {:?}", connection);
                state.remove_client(connection);
            },
            GameNetworkEvent::Message {connection, stream, data } => {
                use utils::BenchmarkPacket;
                let Some(packet) = BenchmarkPacket::from_bytes(data) else {
                    println!("Received invalid packet from client: {:?}", connection);
                    continue;
                };
                println!("Received packet {} from client: {:?}", packet.id, connection);
                server.send(&connection, stream, packet.to_bytes())
            },
            GameNetworkEvent::StreamCreated(_) | GameNetworkEvent::StreamClosed(_) => todo!(),
            GameNetworkEvent::Error { .. } => todo!()
        }
    }
}