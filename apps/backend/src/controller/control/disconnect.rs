use std::sync::Arc;

use crate::{
    controller::controller::PrintableController,
    frame::{Request, RequestData, ResponseData},
    router::RequestContext,
    service::player_service::PlayerService,
};

use crate::controller::controller::Controller;

use crate::model::control::disconnect::DisconnectResponse;
#[derive(Debug, Clone)]
pub struct DisconnectController {
    player_service: Arc<PlayerService>,
}

impl DisconnectController {
    pub fn new(player_service: Arc<PlayerService>) -> Self {
        Self { player_service }
    }
}

impl PrintableController for DisconnectController {}

impl Controller for DisconnectController {
    fn handle_request(
        &self,
        req: Request,
        context: RequestContext,
    ) -> Result<ResponseData, Box<dyn std::error::Error + Send + Sync>> {
        assert!(match *req.get_data() {
            RequestData::Disconnect => true,
            _ => false,
        });
        let player = match self.player_service.get_player(context.client_id) {
            Some(player) => player,
            None => return Err("Player not found".into()),
        };
        self.player_service.remove_player(player)?;
        Ok(ResponseData::Disconnect(DisconnectResponse {
            success: true,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, error::Error};

    use crate::service::{game_service::GameService, lobby_service::LobbyService};

    use super::*;

    #[test]
    fn handle_request_with_user_already_connected_should_be_removed(
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let controller = DisconnectController::new(Arc::new(PlayerService::new(
            Arc::new(LobbyService::new()),
            Arc::new(GameService::new(HashSet::new())),
        )));
        controller
            .player_service
            .add_player(0, String::from("test"));
        controller.handle_request(
            Request::new(0, Arc::new(RequestData::Disconnect)),
            RequestContext { client_id: 0 },
        )?;
        assert!(controller.player_service.get_player(0).is_none());
        Ok(())
    }

    #[test]
    fn handle_request_with_user_not_exist_should_return_error() -> Result<(), Box<dyn Error>> {
        let controller = DisconnectController::new(Arc::new(PlayerService::new(
            Arc::new(LobbyService::new()),
            Arc::new(GameService::new(HashSet::new())),
        )));
        assert!(controller
            .handle_request(
                Request::new(0, Arc::new(RequestData::Disconnect)),
                RequestContext { client_id: 0 }
            )
            .is_err());
        Ok(())
    }
}
