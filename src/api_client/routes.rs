use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
pub enum Endpoint {
    // GET /factions
    GetFactions,
    // POST /register
    PostRegister,
    // GET /my/agent
    GetAgent,
    // GET /my/ships
    GetShipsList,
    // POST /my/ships
    PostBuyShip,
    // GET /my/contracts
    GetContracts,
    // POST /my/contracts/{contract_id}/accept
    PostContractAccept(String),
    // GET /my/ship/{ship_symbol}
    GetShip(String),
    // PATCH /my/ships/{ship_symbol}/nav
    PatchShipNav(String),
    // POST /my/ships/{ship_symbol}/navigate
    PostShipNavigate(String),
    // POST /my/ships/{ship_symbol}/dock
    PostShipDock(String),
    // POST /my/ships/{ship_symbol}/orbit
    PostShipOrbit(String),
    // POST /my/ships/{ship_symbol}/refuel
    PostShipRefuel(String),
    // POST /my/ships/{ship_symbol}/purchase
    PostShipPurchase(String),
    // POST /my/ships/{ship_symbol}/sell
    PostShipSell(String),
    // POST /my/ships/{ship_symbol}/extract/survey
    PostShipExtractSurvey(String),
    // POST /my/ships/{ship_symbol}/jettison
    PostShipJettison(String),
    // POST /my/ships/{ship_symbol}/transfer
    PostShipTransfer(String),
    // POST /my/ships/{ship_symbol}/survey
    PostShipSurvey(String),
    // GET /systems/{system_symbol}
    GetSystem(String),
    // GET /systems/{system_symbol}/waypoints
    GetSystemWaypoints(String),
    // GET /systems/{system_symbol}/waypoints/{waypoint_symbol}/market
    GetWaypointMarket(String, String),
    // GET /systems/{system_symbol}/waypoints/{waypoint_symbol}/shipyard
    GetShipyard(String, String),
    // GET /systems/{system_symbol}/waypoints/{waypoint_symbol}/construction
    GetWaypointConstruction(String, String),
}

impl Endpoint {
    pub fn parse_path(method: &str, path: &str) -> Option<Self> {
        match method {
            "GET" => {
                match path {
                    "/factions" => Some(Endpoint::GetFactions),
                    "/my/agent" => Some(Endpoint::GetAgent),
                    "/my/ships" => Some(Endpoint::GetShipsList),
                    "/my/contracts" => Some(Endpoint::GetContracts),
                    path => {
                        // Try to match /my/ship/{ship_symbol}
                        let ship_re = Regex::new(r"^/my/ship/([^/]+)$").unwrap();
                        if let Some(captures) = ship_re.captures(path) {
                            let ship_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::GetShip(ship_symbol.to_string()))
                        } else if let Some(captures) =
                            Regex::new(r"^/systems/([^/]+)$").unwrap().captures(path)
                        {
                            // Try to match /systems/{system_symbol}
                            let system_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::GetSystem(system_symbol.to_string()))
                        } else if let Some(captures) = Regex::new(r"^/systems/([^/]+)/waypoints$")
                            .unwrap()
                            .captures(path)
                        {
                            // Try to match /systems/{system_symbol}/waypoints
                            let system_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::GetSystemWaypoints(system_symbol.to_string()))
                        } else if let Some(captures) =
                            Regex::new(r"^/systems/([^/]+)/waypoints/([^/]+)/market$")
                                .unwrap()
                                .captures(path)
                        {
                            // Try to match /systems/{system_symbol}/waypoints/{waypoint_symbol}/market
                            let system_symbol = captures.get(1)?.as_str();
                            let waypoint_symbol = captures.get(2)?.as_str();
                            Some(Endpoint::GetWaypointMarket(
                                system_symbol.to_string(),
                                waypoint_symbol.to_string(),
                            ))
                        } else if let Some(captures) =
                            Regex::new(r"^/systems/([^/]+)/waypoints/([^/]+)/shipyard$")
                                .unwrap()
                                .captures(path)
                        {
                            // Try to match /systems/{system_symbol}/waypoints/{waypoint_symbol}/shipyard
                            let system_symbol = captures.get(1)?.as_str();
                            let waypoint_symbol = captures.get(2)?.as_str();
                            Some(Endpoint::GetShipyard(
                                system_symbol.to_string(),
                                waypoint_symbol.to_string(),
                            ))
                        } else if let Some(captures) =
                            Regex::new(r"^/systems/([^/]+)/waypoints/([^/]+)/construction$")
                                .unwrap()
                                .captures(path)
                        {
                            // Try to match /systems/{system_symbol}/waypoints/{waypoint_symbol}/construction
                            let system_symbol = captures.get(1)?.as_str();
                            let waypoint_symbol = captures.get(2)?.as_str();
                            Some(Endpoint::GetWaypointConstruction(
                                system_symbol.to_string(),
                                waypoint_symbol.to_string(),
                            ))
                        } else {
                            None
                        }
                    }
                }
            }
            "POST" => {
                match path {
                    "/register" => Some(Endpoint::PostRegister),
                    "/my/ships" => Some(Endpoint::PostBuyShip),
                    path => {
                        // Try to match contract accept
                        if let Some(captures) = Regex::new(r"^/my/contracts/([^/]+)/accept$")
                            .unwrap()
                            .captures(path)
                        {
                            let contract_id = captures.get(1)?.as_str();
                            Some(Endpoint::PostContractAccept(contract_id.to_string()))
                        } else if let Some(captures) = Regex::new(r"^/my/ships/([^/]+)/navigate$")
                            .unwrap()
                            .captures(path)
                        {
                            // Try to match ship action endpoints
                            let ship_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::PostShipNavigate(ship_symbol.to_string()))
                        } else if let Some(captures) = Regex::new(r"^/my/ships/([^/]+)/dock$")
                            .unwrap()
                            .captures(path)
                        {
                            let ship_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::PostShipDock(ship_symbol.to_string()))
                        } else if let Some(captures) = Regex::new(r"^/my/ships/([^/]+)/orbit$")
                            .unwrap()
                            .captures(path)
                        {
                            let ship_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::PostShipOrbit(ship_symbol.to_string()))
                        } else if let Some(captures) = Regex::new(r"^/my/ships/([^/]+)/refuel$")
                            .unwrap()
                            .captures(path)
                        {
                            let ship_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::PostShipRefuel(ship_symbol.to_string()))
                        } else if let Some(captures) = Regex::new(r"^/my/ships/([^/]+)/purchase$")
                            .unwrap()
                            .captures(path)
                        {
                            let ship_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::PostShipPurchase(ship_symbol.to_string()))
                        } else if let Some(captures) = Regex::new(r"^/my/ships/([^/]+)/sell$")
                            .unwrap()
                            .captures(path)
                        {
                            let ship_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::PostShipSell(ship_symbol.to_string()))
                        } else if let Some(captures) =
                            Regex::new(r"^/my/ships/([^/]+)/extract/survey$")
                                .unwrap()
                                .captures(path)
                        {
                            let ship_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::PostShipExtractSurvey(ship_symbol.to_string()))
                        } else if let Some(captures) = Regex::new(r"^/my/ships/([^/]+)/jettison$")
                            .unwrap()
                            .captures(path)
                        {
                            let ship_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::PostShipJettison(ship_symbol.to_string()))
                        } else if let Some(captures) = Regex::new(r"^/my/ships/([^/]+)/transfer$")
                            .unwrap()
                            .captures(path)
                        {
                            let ship_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::PostShipTransfer(ship_symbol.to_string()))
                        } else if let Some(captures) = Regex::new(r"^/my/ships/([^/]+)/survey$")
                            .unwrap()
                            .captures(path)
                        {
                            let ship_symbol = captures.get(1)?.as_str();
                            Some(Endpoint::PostShipSurvey(ship_symbol.to_string()))
                        } else {
                            None
                        }
                    }
                }
            }
            "PATCH" => {
                // Try to match ship nav update
                if let Some(captures) = Regex::new(r"^/my/ships/([^/]+)/nav$")
                    .unwrap()
                    .captures(path)
                {
                    let ship_symbol = captures.get(1)?.as_str();
                    Some(Endpoint::PatchShipNav(ship_symbol.to_string()))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

pub fn endpoint(method: &str, path: &str) -> Option<Endpoint> {
    Endpoint::parse_path(method, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_endpoint_exact() {
        assert_eq!(endpoint("GET", "/my/ships"), Some(Endpoint::GetShipsList));
        assert_eq!(endpoint("POST", "/my/ships"), Some(Endpoint::PostBuyShip));
        assert_eq!(endpoint("GET", "/factions"), Some(Endpoint::GetFactions));
        assert_eq!(endpoint("POST", "/register"), Some(Endpoint::PostRegister));
        assert_eq!(endpoint("GET", "/my/agent"), Some(Endpoint::GetAgent));
        assert_eq!(
            endpoint("GET", "/my/contracts"),
            Some(Endpoint::GetContracts)
        );
    }
    #[test]
    fn test_endpoint_regex() {
        assert_eq!(
            endpoint("GET", "/my/ship/ABC123"),
            Some(Endpoint::GetShip("ABC123".to_string()))
        );
        assert_eq!(
            endpoint("POST", "/my/ships/XYZ/navigate"),
            Some(Endpoint::PostShipNavigate("XYZ".to_string()))
        );
        assert_eq!(
            endpoint("POST", "/my/ships/XYZ/dock"),
            Some(Endpoint::PostShipDock("XYZ".to_string()))
        );
        assert_eq!(
            endpoint("POST", "/my/ships/XYZ/orbit"),
            Some(Endpoint::PostShipOrbit("XYZ".to_string()))
        );
        assert_eq!(
            endpoint("POST", "/my/ships/XYZ/refuel"),
            Some(Endpoint::PostShipRefuel("XYZ".to_string()))
        );
        assert_eq!(
            endpoint("POST", "/my/ships/XYZ/purchase"),
            Some(Endpoint::PostShipPurchase("XYZ".to_string()))
        );
        assert_eq!(
            endpoint("POST", "/my/ships/XYZ/sell"),
            Some(Endpoint::PostShipSell("XYZ".to_string()))
        );
        assert_eq!(
            endpoint("POST", "/my/ships/XYZ/extract/survey"),
            Some(Endpoint::PostShipExtractSurvey("XYZ".to_string()))
        );
        assert_eq!(
            endpoint("POST", "/my/ships/XYZ/jettison"),
            Some(Endpoint::PostShipJettison("XYZ".to_string()))
        );
        assert_eq!(
            endpoint("POST", "/my/ships/XYZ/transfer"),
            Some(Endpoint::PostShipTransfer("XYZ".to_string()))
        );
        assert_eq!(
            endpoint("POST", "/my/ships/XYZ/survey"),
            Some(Endpoint::PostShipSurvey("XYZ".to_string()))
        );
        assert_eq!(
            endpoint("PATCH", "/my/ships/XYZ/nav"),
            Some(Endpoint::PatchShipNav("XYZ".to_string()))
        );
        assert_eq!(
            endpoint("POST", "/my/contracts/contract123/accept"),
            Some(Endpoint::PostContractAccept("contract123".to_string()))
        );
    }
    #[test]
    fn test_endpoint_two_params() {
        assert_eq!(
            endpoint("GET", "/systems/X1-SR41/waypoints/X1-SR41-A2/shipyard"),
            Some(Endpoint::GetShipyard(
                "X1-SR41".to_string(),
                "X1-SR41-A2".to_string()
            ))
        );
        assert_eq!(
            endpoint("GET", "/systems/X1-SR41/waypoints/X1-SR41-A1/market"),
            Some(Endpoint::GetWaypointMarket(
                "X1-SR41".to_string(),
                "X1-SR41-A1".to_string()
            ))
        );
        assert_eq!(
            endpoint("GET", "/systems/X1-SR41/waypoints/X1-SR41-I60/construction"),
            Some(Endpoint::GetWaypointConstruction(
                "X1-SR41".to_string(),
                "X1-SR41-I60".to_string()
            ))
        );
    }
    #[test]
    fn test_endpoint_system_waypoints() {
        assert_eq!(
            endpoint("GET", "/systems/X1-SR41"),
            Some(Endpoint::GetSystem("X1-SR41".to_string()))
        );
        assert_eq!(
            endpoint("GET", "/systems/X1-SR41/waypoints"),
            Some(Endpoint::GetSystemWaypoints("X1-SR41".to_string()))
        );
    }
    #[test]
    fn test_endpoint_none() {
        assert_eq!(endpoint("GET", "/not/a/route"), None);
        assert_eq!(endpoint("POST", "/my/ships/XYZ/unknown"), None);
        assert_eq!(endpoint("PUT", "/my/ships/XYZ"), None);
    }
}
