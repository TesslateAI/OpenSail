//! Pure `plan(desired, local, observed)` for Fabric product resources.
//!
//! Executors and Kubernetes adapters live outside this module. The live
//! machine only validates that observations match reality.

pub mod database;
pub mod database_run;
pub mod deployment;
pub mod deployment_run;
pub mod routes;
pub mod workspace;
pub mod workspace_run;
