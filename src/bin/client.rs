use game_sockets::{GameNetworkEvent, GamePeer, GameSocketError};
use game_sockets::protocols::UdpProtocol;

fn main() -> Result<(), GameSocketError>{
    let protocol = UdpProtocol::new();
    let mut client = GamePeer::new(protocol);
    client.connect("127.0.0.1", 8080)?;

    loop {
        let event =   client.poll()?;
        let Some(event) = event else {
            continue;
        };

        match event {
            GameNetworkEvent::Connected(connection) => {
                println!("Connected to server: {:?}", connection);
                client.send(connection, 0u16.into(), "Hello Server!".into())
            }
            GameNetworkEvent::Disconnected(connection) => {
                println!("Disconnected from server: {:?}", connection);
            }
            GameNetworkEvent::Message { connection, stream, data } => {
                println!("Received message {:?} from stream {:?} on connection {:?}", data, stream, connection);
            }
            GameNetworkEvent::Error { connection } => {
                println!("Error on connection {:?}", connection);
            }
            GameNetworkEvent::StreamCreated(_) => {}
            GameNetworkEvent::StreamClosed(_) => {}
        }
    }
}