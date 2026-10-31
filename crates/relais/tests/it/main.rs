//! The integration tests, built as one binary so that the whole suite shares
//! one process-wide cap on live worlds.

mod common;
mod dataset_seed;
mod decide_actor;
// The fake worker scripts are Unix shell; every module that uses them is
// Unix-only too.
#[cfg(unix)]
mod fake_bin;
mod feedback_cli;
mod holdout_report;
mod hook_event_fixtures;
mod hook_respond;
mod learning_cycle;
mod native_cancel;
mod native_status;
mod native_usage;
mod no_headless_launch;
mod plugin_agents;
mod plugin_install;
mod portable_scenarios;
mod quiet_watch;
mod release_scenarios;
mod review_outcome;
mod review_parts_cli;
mod session_router;
mod successor_intent;
mod task_type_pin;
mod task_types_cli;
