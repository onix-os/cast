// @generated automatically by Diesel CLI.

diesel::table! {
    state (id) {
        id -> Integer,
        #[sql_name = "type"]
        type_ -> Text,
        created -> BigInt,
        summary -> Nullable<Text>,
        description -> Nullable<Text>,
        transition_id -> Nullable<Text>,
    }
}

diesel::table! {
    state_selections (state_id, package_id) {
        state_id -> Integer,
        package_id -> Text,
        explicit -> Bool,
        reason -> Nullable<Text>,
    }
}

diesel::table! {
    state_metadata_provenance (state_id) {
        state_id -> Integer,
        os_release_sha256 -> Binary,
        system_model_sha256 -> Binary,
    }
}

diesel::joinable!(state_metadata_provenance -> state (state_id));
diesel::joinable!(state_selections -> state (state_id));

diesel::allow_tables_to_appear_in_same_query!(state, state_metadata_provenance, state_selections,);
