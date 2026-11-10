// needs-a-server: nine modules that create and drop their own database where DATABASE_URL points, cloned from the migrated template (common) or empty (empty_db)
//
// The scratch-database files with no process state, in one test binary: see group_no_server.rs for
// why and for what a module may not do. This root is the one ci.yml reads the first line of, so the
// no-server job leaves out the whole group and not a file at a time.

mod common;
mod empty_db;

#[path = "group_scratch_db/eval_fixture_seeds_through_the_binary.rs"]
mod eval_fixture_seeds_through_the_binary;
#[path = "group_scratch_db/v029_accounts_cli.rs"]
mod v029_accounts_cli;
#[path = "group_scratch_db/v029_accounts_not_in_a_bundle.rs"]
mod v029_accounts_not_in_a_bundle;
#[path = "group_scratch_db/v029_accounts_schema.rs"]
mod v029_accounts_schema;
#[path = "group_scratch_db/v029_accounts_storage.rs"]
mod v029_accounts_storage;
#[path = "group_scratch_db/v043_a_model_of_another_width_does_not_start.rs"]
mod v043_a_model_of_another_width_does_not_start;
#[path = "group_scratch_db/v043_calibration_and_export_read_what_was_written.rs"]
mod v043_calibration_and_export_read_what_was_written;
#[path = "group_scratch_db/v044_a_migration_recorded_in_crlf_still_starts.rs"]
mod v044_a_migration_recorded_in_crlf_still_starts;
#[path = "group_scratch_db/v044_a_new_app_role_does_not_get_a_published_password.rs"]
mod v044_a_new_app_role_does_not_get_a_published_password;
