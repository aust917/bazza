//! Real Windows plugin admission runs separately from the native unit harness.
//! This target receives its own Common Controls activation manifest.
#![cfg(windows)]

#[path = "../src/startup.rs"]
mod startup;
#[path = "../src/startup_gate_tests.rs"]
mod startup_gate_tests;
