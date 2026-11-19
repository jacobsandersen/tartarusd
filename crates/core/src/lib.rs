//! Pure model and state machine for tartarusd. No I/O in this crate.

pub mod action;
pub mod config;
pub mod keys;
mod keys_table;
pub mod machine;
pub mod profile;

pub use action::{Action, ActionError, Step};
pub use config::{Config, ConfigError};
pub use keys::{Edge, Key, PhysicalKey, UnknownKey, WheelDir};
pub use machine::{Effect, Event, Machine};
pub use profile::{MatchRule, Mode, Profile, ProfileError};
