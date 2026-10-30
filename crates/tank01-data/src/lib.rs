//! Tank01 live data polling, capture, and replay.
//!
//! Captures written before the `game_id` / `tank01_game_id` rename and nested
//! `season_week` representation are not backward compatible with current replay.
//! Historical captures must be converted before replaying them.

mod capture;
mod client;
mod config;
mod injuries;
mod normalize;
mod poll;

mod replay;
pub use capture::{CaptureError, CaptureReport, CaptureWriter};
pub use client::{Tank01Client, Tank01ClientError};
pub use config::Tank01Config;
pub use poll::{CompletionReason, PollEvent, PollRequest, PollerError, SlatePoller};
pub use replay::{Replay, ReplayError, ReplayMismatch, ReplayReport};
