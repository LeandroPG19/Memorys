// One test binary for the files that hold no process state and need no server. Each one used to be a
// `tests/<name>.rs` of its own, and every binary is a link of the whole crate. The folder holds the
// modules: cargo builds a `tests/*.rs` and never looks inside a folder, so a module exists only
// through this root, and `smoke_test` checks that the two agree and that none takes what a shared
// process cannot give (`a_grouped_test_holds_no_process_wide_state`). A file that writes the
// environment, takes a static guard or an advisory lock, is `#[ignore]`, asserts a clock or has a
// ceiling of its own stays out of the group.

mod dead_db;

#[path = "group_no_server/cli_contract.rs"]
mod cli_contract;
#[path = "group_no_server/connect_surface_contract.rs"]
mod connect_surface_contract;
#[path = "group_no_server/doc_contract.rs"]
mod doc_contract;
#[path = "group_no_server/e2e_tool_matrix_contract.rs"]
mod e2e_tool_matrix_contract;
#[path = "group_no_server/gate_lane_token_contract.rs"]
mod gate_lane_token_contract;
#[path = "group_no_server/migrations_contract.rs"]
mod migrations_contract;
#[path = "group_no_server/scram_verifier_contract.rs"]
mod scram_verifier_contract;
#[path = "group_no_server/sql_safe_contract.rs"]
mod sql_safe_contract;
#[path = "group_no_server/v026_the_panel_is_not_a_second_door.rs"]
mod v026_the_panel_is_not_a_second_door;
#[path = "group_no_server/v029_apply_keeps_what_the_operator_set.rs"]
mod v029_apply_keeps_what_the_operator_set;
#[path = "group_no_server/v029_auto_client_cli.rs"]
mod v029_auto_client_cli;
#[path = "group_no_server/v029_auto_host_doctor.rs"]
mod v029_auto_host_doctor;
#[path = "group_no_server/v029_auto_host_follows_its_ip.rs"]
mod v029_auto_host_follows_its_ip;
#[path = "group_no_server/v029_auto_host_serve_accepts_lan.rs"]
mod v029_auto_host_serve_accepts_lan;
#[path = "group_no_server/v029_bridge_cli.rs"]
mod v029_bridge_cli;
#[path = "group_no_server/v042_placement_is_a_decision_not_a_hope.rs"]
mod v042_placement_is_a_decision_not_a_hope;
#[path = "group_no_server/v045_a_git_merge_of_two_exports.rs"]
mod v045_a_git_merge_of_two_exports;
#[path = "group_no_server/v048_the_start_opens_one_pool.rs"]
mod v048_the_start_opens_one_pool;
