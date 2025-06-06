// @generated automatically by Diesel CLI.

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
    market_transaction_log (id) {
        id -> Int8,
        timestamp -> Timestamptz,
        market_symbol -> Text,
        symbol -> Text,
        ship_symbol -> Text,
        #[sql_name = "type"]
        type_ -> Text,
        units -> Int4,
        price_per_unit -> Int4,
        total_price -> Int4,
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
    generic_lookup,
    jumpgate_connections,
    market_transaction_log,
    markets,
    remote_markets,
    remote_shipyards,
    shipyards,
    surveys,
    systems,
    waypoint_details,
    waypoints,
);
