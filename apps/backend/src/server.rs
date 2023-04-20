use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;
use std::time::Instant;

use crate::connection::Connection;
use crate::frame::{Frame, Request, Response};
use crate::lobby::lobbies::Lobbies;
use crate::lobby::lobby::Lobby;
use crate::model::control::{
    connect::ConnectResponse, disconnect::DisconnectResponse, heartbeat::HeartbeatResponse,
};
use crate::player::Player;
use priority_queue::PriorityQueue;
use tokio::net::TcpListener;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct Server {
    player_timeout_queue: Arc<Mutex<PriorityQueue<u32, Instant>>>,
    host: String,
    port: u32,
    online_player_map: ClientMap,
    lobbies: Arc<Mutex<Lobbies>>,
    game_map: GameMap,
}

pub struct Context {
    pub opcode: u8,
    pub payload: Vec<u8>,
}

type ClientMap = Arc<Mutex<HashMap<u32, Arc<Mutex<Player>>>>>;
type GameMap = Arc<Mutex<HashMap<u32, Arc<Mutex<Vec<u32>>>>>>;

unsafe impl Send for Server {}
unsafe impl Sync for Server {}

impl Server {
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind(format!("{}:{}", self.host, self.port)).await?;

        let mut next_client_id = 0;

        loop {
            let (socket, _) = listener.accept().await?;
            let (tx, mut rx): (Sender<Frame>, Receiver<Frame>) = channel(128);

            let client_id = next_client_id;
            next_client_id += 1;

            // clone the map
            let connection = Arc::new(Mutex::new(Connection::new(socket)));
            let connection_receiver = connection.clone();
            let connection_sender = connection.clone();
            let server = self.clone();

            tokio::spawn(async move {
                loop {
                    let frame = match connection_receiver.lock().await.try_read_frame() {
                        Ok(Some(frame)) => frame,
                        Ok(None) => {
                            continue;
                        }
                        Err(e) => {
                            eprintln!("failed to read frame; err = {:?}", e);
                            break;
                        }
                    };
                    match frame {
                        Frame::Request(req) => {
                            let result = server
                                .handle_request(
                                    client_id,
                                    tx.clone(),
                                    #[cfg(not(test))]
                                    connection_receiver.clone(),
                                    req,
                                )
                                .await;
                            if result.is_err() {
                                eprintln!("failed to handle request; err = {:?}", result);
                            }
                        }
                        Frame::Error(_) | Frame::Response(_) => {
                            eprintln!("invalid frame; frame = {:?}", frame)
                        }
                    };
                }
            });

            tokio::spawn(async move {
                loop {
                    while let Some(frame) = rx.recv().await {
                        println!("received frame; frame = {:?}", frame);
                        let mut connection = connection_sender.lock().await;
                        // println!("get connection = {:?}", connection);
                        match connection.write_frame(&frame).await {
                            Ok(_) => {
                                println!("sent frame; frame = {:?}", frame);
                                continue;
                            }
                            Err(e) => {
                                eprintln!("failed to write frame; err = {:?}", e);
                                break;
                            }
                        }
                    }
                }
            });
        }
    }

    #[cfg(not(test))]
    pub fn new(host: String, port: u32) -> Self {
        Server {
            player_timeout_queue: Arc::new(Mutex::new(PriorityQueue::new())),
            host,
            port,
            online_player_map: ClientMap::new(Mutex::new(HashMap::new())),
            lobbies: Arc::new(Mutex::new(Lobbies::new())),
            game_map: GameMap::new(Mutex::new(HashMap::new())),
        }
    }

    #[cfg(test)]
    pub fn new() -> Self {
        Server {
            player_timeout_queue: Arc::new(Mutex::new(PriorityQueue::new())),
            host: String::from("0.0.0.0"),
            port: 45678,
            online_player_map: ClientMap::new(Mutex::new(HashMap::new())),
            lobbies: Arc::new(Mutex::new(Lobbies::new())),
            game_map: GameMap::new(Mutex::new(HashMap::new())),
        }
    }

    async fn connect(
        &self,
        client_id: u32,
        name: String,
        #[cfg(not(test))] connection: Arc<Mutex<Connection>>,
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        if self.online_player_map.lock().await.contains_key(&client_id) {
            return Err("client already connected".into());
        }

        self.online_player_map.lock().await.insert(
            client_id,
            Arc::new(Mutex::new(Player::new(
                client_id,
                name,
                #[cfg(not(test))]
                connection,
            ))),
        );

        self.player_timeout_queue
            .lock()
            .await
            .push(client_id, Instant::now());

        Ok(())
    }

    async fn disconnect(&self, client_id: u32) -> Result<(), Box<dyn Error + Sync + Send>> {
        match self.online_player_map.lock().await.remove(&client_id) {
            Some(player) => {
                let player = player.lock().await;
                self.player_timeout_queue.lock().await.remove(&client_id);
                if player.lobby_id.is_some() {
                    let lobby = self
                        .lobbies
                        .clone()
                        .lock()
                        .await
                        .get_lobby(player.lobby_id.unwrap())
                        .await
                        .unwrap();
                    lobby.lock().await.remove_player(player.id).await;
                }
                if player.game_id.is_some() {
                    let game = self.game_map.lock().await[&player.game_id.unwrap()].clone();
                    game.lock().await.retain(|&x| x != client_id);
                }
                Ok(())
            }
            None => Err("Player not found")?,
        }
    }

    async fn heartbeat(&self, client_id: u32) -> Result<(), Box<dyn Error + Sync + Send>> {
        match self
            .player_timeout_queue
            .lock()
            .await
            .change_priority(&client_id, Instant::now())
        {
            Some(_) => Ok(()),
            None => Err("Player not found")?,
        }
    }

    async fn create_lobby(
        &self,
        client_id: u32,
    ) -> Result<Arc<Mutex<Lobby>>, Box<dyn Error + Sync + Send>> {
        let lobby = self.lobbies.lock().await.create_lobby().await;
        let players = self.online_player_map.lock().await;
        let player = players.get(&client_id);
        if player.is_none() {
            return Err("Player not found".into());
        }
        match lobby
            .clone()
            .lock()
            .await
            .add_player(player.unwrap().clone())
            .await
        {
            Ok(_) => Ok(lobby),
            Err(e) => Err(e),
        }
    }

    async fn join_lobby(
        &self,
        client_id: u32,
        lobby_id: u32,
    ) -> Result<Arc<Mutex<Lobby>>, Box<dyn Error + Sync + Send>> {
        let lobby = self.lobbies.clone().lock().await.get_lobby(lobby_id).await;

        if lobby.is_none() {
            return Err("Lobby not found".into());
        }

        let players = self.online_player_map.lock().await;
        let player = players.get(&client_id);

        if player.is_none() {
            return Err("Player not found".into());
        }

        let lobby = lobby.unwrap().clone();
        match lobby
            .clone()
            .lock()
            .await
            .add_player(player.unwrap().clone())
            .await
        {
            Ok(_) => Ok(lobby),
            Err(e) => return Err(e),
        }
    }

    async fn quit_lobby(&self, client_id: u32) -> Result<(), Box<dyn Error + Sync + Send>> {
        let players = self.online_player_map.lock().await;
        let player = players.get(&client_id);

        if player.is_none() {
            return Err("Player not found".into());
        }

        let lobby_id = player.unwrap().lock().await.lobby_id;

        if lobby_id.is_none() {
            return Err("Player not in lobby".into());
        }

        let lobby = self
            .lobbies
            .clone()
            .lock()
            .await
            .get_lobby(lobby_id.unwrap())
            .await;

        if lobby.is_none() {
            return Err("Lobby not found".into());
        }

        lobby.unwrap().lock().await.remove_player(client_id).await;
        Ok(())
    }

    async fn handle_request(
        &self,
        client_id: u32,
        tx: Sender<Frame>,
        #[cfg(not(test))] connection: Arc<Mutex<Connection>>,
        request: Request,
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        match request {
            Request::Connect(req) => match self
                .connect(
                    client_id,
                    req.name,
                    #[cfg(not(test))]
                    connection,
                )
                .await
            {
                Ok(_) => {
                    tx.send(Frame::Response(Response::Connect(ConnectResponse {
                        success: true,
                    })))
                    .await?;
                    Ok(())
                }
                Err(e) => {
                    tx.send(Frame::Response(Response::Connect(ConnectResponse {
                        success: false,
                    })))
                    .await?;
                    Err(e)
                }
            },
            Request::Disconnect => match self.disconnect(client_id).await {
                Ok(_) => {
                    tx.send(Frame::Response(Response::Disconnect(DisconnectResponse {
                        success: true,
                    })))
                    .await?;
                    Ok(())
                }
                Err(e) => {
                    tx.send(Frame::Response(Response::Disconnect(DisconnectResponse {
                        success: false,
                    })))
                    .await?;
                    Err(e)
                }
            },
            Request::Heartbeat => match self.heartbeat(client_id).await {
                Ok(_) => {
                    tx.send(Frame::Response(Response::Heartbeat(HeartbeatResponse {
                        success: true,
                    })))
                    .await?;
                    Ok(())
                }
                Err(e) => {
                    tx.send(Frame::Response(Response::Heartbeat(HeartbeatResponse {
                        success: false,
                    })))
                    .await?;
                    Err(e)
                }
            },
            Request::CreateLobby => match self.create_lobby(client_id).await {
                Ok(lobby) => {
                    tx.send(Frame::Response(Response::CreateLobby(
                        crate::model::lobby::create::CreateResponse {
                            success: true,
                            lobby: Some(crate::model::lobby::lobby::Lobby::from_lobby(lobby).await),
                        },
                    )))
                    .await?;
                    Ok(())
                }
                Err(e) => {
                    tx.send(Frame::Response(Response::CreateLobby(
                        crate::model::lobby::create::CreateResponse {
                            success: false,
                            lobby: None,
                        },
                    )))
                    .await?;
                    Err(e)
                }
            },
            Request::JoinLobby(req) => match self.join_lobby(client_id, req.lobby_id).await {
                Ok(res) => {
                    tx.send(Frame::Response(Response::JoinLobby(
                        crate::model::lobby::join::JoinResponse {
                            success: true,
                            lobby: Some(crate::model::lobby::lobby::Lobby::from_lobby(res).await),
                        },
                    )))
                    .await?;
                    Ok(())
                }
                Err(e) => {
                    tx.send(Frame::Response(Response::JoinLobby(
                        crate::model::lobby::join::JoinResponse {
                            success: false,
                            lobby: None,
                        },
                    )))
                    .await?;
                    Err(e)
                }
            },

            Request::QuitLobby => match self.quit_lobby(client_id).await {
                Ok(_) => {
                    tx.send(Frame::Response(Response::QuitLobby(
                        crate::model::lobby::quit::QuitResponse { success: true },
                    )))
                    .await?;
                    Ok(())
                }
                Err(e) => {
                    tx.send(Frame::Response(Response::QuitLobby(
                        crate::model::lobby::quit::QuitResponse { success: false },
                    )))
                    .await?;
                    Err(e)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_with_test_user_online_player_map_should_include_test_user(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        server.connect(0, String::from("test")).await?;
        let online_player_map = server.online_player_map.lock().await;
        let player = online_player_map.get(&0).unwrap().lock().await;
        assert_eq!(player.id, 0);
        assert_eq!(player.name, String::from("test"));
        Ok(())
    }

    #[tokio::test]
    async fn connect_with_test_user_player_timeout_queue_should_include_test_user(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        server.connect(0, String::from("test")).await?;
        let player_timeout_queue = server.player_timeout_queue.lock().await;
        assert!(player_timeout_queue.get(&0).is_some());
        Ok(())
    }

    #[tokio::test]
    async fn connect_with_test_user_who_already_connected_should_return_error(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        server.connect(0, String::from("test")).await?;
        assert!(server.connect(0, String::from("test")).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_with_user_already_connected_should_be_removed(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        server.online_player_map.lock().await.insert(
            0,
            Arc::new(Mutex::new(Player::new(0, String::from("test")))),
        );
        server
            .player_timeout_queue
            .lock()
            .await
            .push(0, Instant::now());
        server.disconnect(0).await?;
        assert!(server.online_player_map.lock().await.len() == 0);
        assert!(server.player_timeout_queue.lock().await.len() == 0);
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_with_user_not_exist_should_return_error(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        assert!(server.disconnect(0).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn create_lobby_with_test_user_should_create_lobby(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        server.online_player_map.lock().await.insert(
            0,
            Arc::new(Mutex::new(Player::new(0, String::from("test")))),
        );
        server.create_lobby(0).await?;
        assert!(server.lobbies.lock().await.get_lobby(0).await.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn create_lobby_with_not_exist_user_should_return_error(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        assert!(server.create_lobby(0).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn create_lobby_with_test_user_should_contains_test_user(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        server.online_player_map.lock().await.insert(
            0,
            Arc::new(Mutex::new(Player::new(0, String::from("test")))),
        );
        server.create_lobby(0).await?;
        assert!(server
            .lobbies
            .lock()
            .await
            .get_lobby(0)
            .await
            .unwrap()
            .lock()
            .await
            .get_player(0)
            .await
            .is_some());
        Ok(())
    }

    #[tokio::test]
    async fn join_lobby_with_test_user_and_test_lobby_should_join_lobby(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        server.online_player_map.lock().await.insert(
            0,
            Arc::new(Mutex::new(Player::new(0, String::from("test")))),
        );
        server.lobbies.lock().await.create_lobby().await;
        server.join_lobby(0, 0).await?;
        assert!(server
            .lobbies
            .lock()
            .await
            .get_lobby(0)
            .await
            .unwrap()
            .lock()
            .await
            .get_player(0)
            .await
            .is_some());
        Ok(())
    }

    #[tokio::test]
    async fn join_lobby_with_not_exist_user_and_test_lobby_should_return_error(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        server.online_player_map.lock().await.insert(
            0,
            Arc::new(Mutex::new(Player::new(0, String::from("test")))),
        );
        server.create_lobby(0).await?;
        assert!(server.join_lobby(1, 0).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn join_lobby_with_test_user_and_not_exist_lobby_should_return_error(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        server.online_player_map.lock().await.insert(
            0,
            Arc::new(Mutex::new(Player::new(0, String::from("test")))),
        );
        assert!(server.join_lobby(0, 0).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn join_lobby_with_not_exist_user_and_not_exist_lobby_should_return_error(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        assert!(server.join_lobby(0, 0).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn quit_lobby_with_test_user_in_test_lobby_should_quit_lobby(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        server.online_player_map.lock().await.insert(
            0,
            Arc::new(Mutex::new(Player::new(0, String::from("test")))),
        );
        let lobby = server.lobbies.lock().await.create_lobby().await;
        let player = Arc::new(Mutex::new(Player::new(0, String::from("test"))));
        server
            .online_player_map
            .lock()
            .await
            .insert(0, player.clone());
        lobby.lock().await.add_player(player).await?;
        server.quit_lobby(0).await?;
        assert!(server
            .lobbies
            .lock()
            .await
            .get_lobby(0)
            .await
            .unwrap()
            .lock()
            .await
            .get_player(0)
            .await
            .is_none());
        Ok(())
    }

    #[tokio::test]
    async fn quit_lobby_with_not_exist_user_should_return_error(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        assert!(server.quit_lobby(0).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn quit_lobby_with_test_user_but_not_in_lobby_should_return_error(
    ) -> Result<(), Box<dyn Error + Sync + Send>> {
        let server = Server::new();
        server.online_player_map.lock().await.insert(
            0,
            Arc::new(Mutex::new(Player::new(0, String::from("test")))),
        );
        let player = Arc::new(Mutex::new(Player::new(0, String::from("test"))));
        server
            .online_player_map
            .lock()
            .await
            .insert(0, player.clone());
        assert!(server.quit_lobby(0).await.is_err());
        Ok(())
    }
}
