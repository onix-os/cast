use crate::{state, transition_identity::MAX_ARCHIVED_STATE_PRUNE_BATCH};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RemovalToken {
    start: i32,
    end: i32,
    count: usize,
}

pub(super) fn parse_state_id(value: &str) -> Result<state::Id, Error> {
    parse_state_id_number(value).map(state::Id::from)
}

pub(super) fn parse_removal_token(value: &str) -> Result<RemovalToken, Error> {
    let (start, end) = match value.split_once('-') {
        Some((start, end)) => (parse_state_id_number(start)?, parse_state_id_number(end)?),
        None => {
            let id = parse_state_id_number(value)?;
            (id, id)
        }
    };
    if start > end {
        return Err(Error::DescendingRange { start, end });
    }

    let count =
        u64::try_from(i64::from(end) - i64::from(start) + 1).map_err(|_| Error::DescendingRange { start, end })?;
    let count = match usize::try_from(count) {
        Ok(count) => count,
        Err(_) => {
            return Err(Error::TooManyIds {
                actual: count,
                limit: MAX_ARCHIVED_STATE_PRUNE_BATCH,
            });
        }
    };
    if count > MAX_ARCHIVED_STATE_PRUNE_BATCH {
        return Err(Error::TooManyIds {
            actual: usize_to_u64(count),
            limit: MAX_ARCHIVED_STATE_PRUNE_BATCH,
        });
    }

    Ok(RemovalToken { start, end, count })
}

pub(super) fn collect_removal_ids<'a>(
    requested: impl IntoIterator<Item = &'a RemovalToken>,
) -> Result<Vec<state::Id>, Error> {
    let mut retained_tokens = Vec::<RemovalToken>::new();
    let mut total = 0_usize;
    for token in requested {
        if token.count > MAX_ARCHIVED_STATE_PRUNE_BATCH - total {
            let actual = usize_to_u64(total.saturating_add(token.count));
            return Err(Error::TooManyIds {
                actual,
                limit: MAX_ARCHIVED_STATE_PRUNE_BATCH,
            });
        }
        total += token.count;
        retained_tokens.push(*token);
    }
    if retained_tokens.is_empty() {
        return Err(Error::MissingIds);
    }

    let mut ids = Vec::with_capacity(total);
    for token in retained_tokens {
        for id in token.start..=token.end {
            ids.push(state::Id::from(id));
        }
    }
    Ok(ids)
}

fn parse_state_id_number(value: &str) -> Result<i32, Error> {
    let id = value.parse::<i32>().map_err(|_| Error::InvalidId {
        value: value.to_owned(),
    })?;
    if id <= 0 || value != id.to_string() {
        return Err(Error::InvalidId {
            value: value.to_owned(),
        });
    }
    Ok(id)
}

fn usize_to_u64(value: usize) -> u64 {
    match u64::try_from(value) {
        Ok(value) => value,
        Err(_) => u64::MAX,
    }
}

#[derive(Debug, Error)]
pub enum StateRequestError {
    #[error("state ID `{value}` must be one canonical positive decimal i32")]
    InvalidId { value: String },
    #[error("state range start {start} exceeds end {end}")]
    DescendingRange { start: i32, end: i32 },
    #[error("state removal request expands to {actual} IDs; maximum is {limit}")]
    TooManyIds { actual: u64, limit: usize },
    #[error("state removal request contains no IDs")]
    MissingIds,
}

type Error = StateRequestError;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Installation, cli::state as state_cli, test_support::private_installation_tempdir};

    const WRAPPING_ALIAS_FOR_STATE_ONE: &str = "4294967297";

    #[test]
    fn state_request_accepts_canonical_positive_i32_boundaries() {
        assert_eq!(parse_state_id("1").unwrap(), state::Id::from(1));
        assert_eq!(
            parse_state_id(&i32::MAX.to_string()).unwrap(),
            state::Id::from(i32::MAX)
        );
    }

    #[test]
    fn state_request_rejects_noncanonical_out_of_range_and_aliasing_ids() {
        for value in ["", "0", "-1", "+1", "01", "not-a-state", WRAPPING_ALIAS_FOR_STATE_ONE] {
            assert!(
                matches!(parse_state_id(value), Err(Error::InvalidId { .. })),
                "accepted {value:?}"
            );
        }
    }

    #[test]
    fn state_request_accepts_exact_bounded_removal_range() {
        let token = parse_removal_token("1-64").unwrap();
        let ids = collect_removal_ids([&token]).unwrap();

        assert_eq!(ids.len(), MAX_ARCHIVED_STATE_PRUNE_BATCH);
        assert_eq!(ids.first(), Some(&state::Id::from(1)));
        assert_eq!(ids.last(), Some(&state::Id::from(64)));
    }

    #[test]
    fn state_request_rejects_descending_and_oversized_ranges_before_expansion() {
        assert!(matches!(
            parse_removal_token("2-1"),
            Err(Error::DescendingRange { start: 2, end: 1 })
        ));
        assert!(matches!(
            parse_removal_token("1-2147483647"),
            Err(Error::TooManyIds { actual: 2_147_483_647, limit })
                if limit == MAX_ARCHIVED_STATE_PRUNE_BATCH
        ));
        assert!(matches!(
            parse_removal_token(&format!("1-{WRAPPING_ALIAS_FOR_STATE_ONE}")),
            Err(Error::InvalidId { .. })
        ));
    }

    #[test]
    fn state_request_rejects_aggregate_n_plus_one_before_materialization() {
        let full = parse_removal_token("1-64").unwrap();
        let extra = parse_removal_token("65").unwrap();

        assert!(matches!(
            collect_removal_ids([&full, &extra]),
            Err(Error::TooManyIds { actual: 65, limit }) if limit == MAX_ARCHIVED_STATE_PRUNE_BATCH
        ));
    }

    #[test]
    fn state_command_parser_rejects_invalid_ids_for_every_state_subcommand() {
        for arguments in [
            ["state", "activate", WRAPPING_ALIAS_FOR_STATE_ONE],
            ["state", "query", WRAPPING_ALIAS_FOR_STATE_ONE],
            ["state", "remove", WRAPPING_ALIAS_FOR_STATE_ONE],
            ["state", "export", WRAPPING_ALIAS_FOR_STATE_ONE],
        ] {
            assert!(
                state_cli::command().try_get_matches_from(arguments).is_err(),
                "accepted {arguments:?}"
            );
        }
        for arguments in [
            ["state", "activate", "--", "-1"],
            ["state", "query", "--", "-1"],
            ["state", "export", "--", "-1"],
        ] {
            assert!(
                state_cli::command().try_get_matches_from(arguments).is_err(),
                "accepted {arguments:?}"
            );
        }
    }

    #[test]
    fn state_remove_aggregate_rejection_precedes_client_database_creation() {
        let matches = state_cli::command()
            .try_get_matches_from(["state", "remove", "1-64", "65"])
            .unwrap();
        let remove = matches.subcommand_matches("remove").unwrap();
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let database_paths = [
            installation.db_path("install"),
            installation.db_path("state"),
            installation.db_path("layout"),
        ];
        assert!(database_paths.iter().all(|path| !path.exists()));

        let error = state_cli::remove(remove, installation, true, false).unwrap_err();

        assert!(matches!(
            error,
            state_cli::Error::Request(Error::TooManyIds { actual: 65, limit })
                if limit == MAX_ARCHIVED_STATE_PRUNE_BATCH
        ));
        assert!(database_paths.iter().all(|path| !path.exists()));
    }
}
