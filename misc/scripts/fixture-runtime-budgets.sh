# Shared wall-clock budgets for the nested fixture evidence campaign.
# This file is sourced by both runners so the outer service cannot silently
# fall back to the same deadline as the all-fixture service it contains.

fixture_budget_seconds_per_hour=3600
fixture_budget_preflight_runtime_seconds=30
fixture_budget_single_runtime_seconds=$((2 * fixture_budget_seconds_per_hour))
fixture_budget_all_default_runtime_seconds=$((4 * fixture_budget_seconds_per_hour))
fixture_budget_all_max_runtime_seconds=$((5 * fixture_budget_seconds_per_hour))

# At the largest supported inner budget, the outer campaign still owns a full
# hour for preparation, the inner client's stop/reap envelope, proof validation,
# and evidence publication.
fixture_budget_outer_minimum_headroom_seconds=$fixture_budget_seconds_per_hour
fixture_budget_ci_default_runtime_seconds=$((6 * fixture_budget_seconds_per_hour))

# These margins begin only after a service runtime expires. Kill-after remains
# separate because operators may tighten it for bounded failure tests.
fixture_budget_preflight_client_completion_margin_seconds=5
fixture_budget_delegated_client_completion_margin_seconds=60
fixture_budget_ci_client_completion_margin_seconds=10
fixture_budget_status_delivery_margin_seconds=5

fixture_budget_max_kill_after_seconds=300
fixture_budget_ci_max_runtime_seconds=$fixture_budget_ci_default_runtime_seconds
fixture_budget_delegated_max_status_timeout_seconds=$((
    fixture_budget_all_max_runtime_seconds
        + (2 * fixture_budget_max_kill_after_seconds)
        + fixture_budget_delegated_client_completion_margin_seconds
        + fixture_budget_status_delivery_margin_seconds
))
fixture_budget_ci_max_status_timeout_seconds=$((
    fixture_budget_ci_max_runtime_seconds
        + (2 * fixture_budget_max_kill_after_seconds)
        + fixture_budget_ci_client_completion_margin_seconds
        + fixture_budget_status_delivery_margin_seconds
))
