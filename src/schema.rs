// @generated automatically by Diesel CLI.

diesel::table! {
    agent_metrics (ts) {
        ts -> Timestamptz,
        credits -> Int8,
        available_credits -> Int8,
        reserved_credits -> Int8,
        cargo_value -> Int8,
        num_ships -> Int4,
        net_worth -> Int8,
    }
}

diesel::table! {
    agent_transaction_log (id, ts) {
        id -> Int8,
        ts -> Timestamptz,
        #[sql_name = "type"]
        type_ -> Text,
        ship_symbol -> Nullable<Text>,
        reference -> Nullable<Text>,
        waypoint -> Nullable<Text>,
        units -> Nullable<Int4>,
        amount -> Int8,
        realized_profit -> Nullable<Int8>,
    }
}

diesel::table! {
    construction_log (ts, waypoint, trade_symbol) {
        ts -> Timestamptz,
        waypoint -> Text,
        trade_symbol -> Text,
        fulfilled -> Int4,
        required -> Int4,
    }
}

diesel::table! {
    generic_lookup (key) {
        key -> Text,
        value -> Json,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    jumpgate_connections (waypoint_symbol) {
        waypoint_symbol -> Text,
        edges -> Array<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        is_under_construction -> Bool,
    }
}

diesel::table! {
    market_trades (market_symbol, symbol, timestamp) {
        timestamp -> Timestamptz,
        market_symbol -> Text,
        symbol -> Text,
        trade_volume -> Int4,
        #[sql_name = "type"]
        type_ -> Text,
        supply -> Text,
        activity -> Nullable<Text>,
        purchase_price -> Int4,
        sell_price -> Int4,
    }
}

diesel::table! {
    market_observations (market_symbol, timestamp) {
        timestamp -> Timestamptz,
        market_symbol -> Text,
    }
}

diesel::table! {
    markets (waypoint_symbol) {
        waypoint_symbol -> Text,
        market_data -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    remote_markets (waypoint_symbol) {
        waypoint_symbol -> Text,
        market_data -> Json,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    remote_shipyards (waypoint_symbol) {
        waypoint_symbol -> Text,
        shipyard_data -> Json,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    shipyards (waypoint_symbol) {
        waypoint_symbol -> Text,
        shipyard_data -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    surveys (uuid) {
        uuid -> Uuid,
        survey -> Json,
        asteroid_symbol -> Text,
        inserted_at -> Timestamptz,
        expires_at -> Timestamptz,
    }
}

diesel::table! {
    systems (id) {
        id -> Int8,
        symbol -> Text,
        #[sql_name = "type"]
        type_ -> Text,
        x -> Int4,
        y -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    waypoint_details (id) {
        id -> Int8,
        waypoint_id -> Int8,
        is_market -> Bool,
        is_shipyard -> Bool,
        is_uncharted -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        is_under_construction -> Bool,
    }
}

diesel::table! {
    waypoints (id) {
        id -> Int8,
        symbol -> Text,
        system_id -> Int8,
        #[sql_name = "type"]
        type_ -> Text,
        x -> Int4,
        y -> Int4,
        created_at -> Timestamptz,
    }
}

diesel::allow_tables_to_appear_in_same_query!(
    agent_metrics,
    agent_transaction_log,
    construction_log,
    generic_lookup,
    jumpgate_connections,
    market_observations,
    market_trades,
    markets,
    remote_markets,
    remote_shipyards,
    shipyards,
    surveys,
    systems,
    waypoint_details,
    waypoints,
);
