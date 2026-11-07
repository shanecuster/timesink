//! timesink-core: everything about *what* gets recorded, with no Hyprland or UI code.
//!
//! Data flows through the modules in this order:
//!   raw Hyprland window  ->  `sanitize` (privacy rules, uses `title`)
//!                        ->  `model::WindowSnapshot`
//!                        ->  `tracker` (turns snapshots into time intervals)
//!                        ->  `db` (stores the intervals in SQLite)

pub mod appmap;
pub mod config;
pub mod db;
pub mod listen;
pub mod model;
pub mod presence;
pub mod sanitize;
pub mod title;
pub mod tracker;
