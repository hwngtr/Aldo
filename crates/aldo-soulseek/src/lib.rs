//! SoulSeek search.
//!
//! The network has no search index: the server relays a query and peers answer
//! with whatever they hold. Results therefore depend on who is online, and a
//! file on an offline machine is invisible. Search is a shortlist of what is
//! reachable now, not a catalogue.
//!
//! `soulseek-rs-lib` is blocking, so the client runs on its own thread and is
//! reached over channels. It also cannot be proxied: it owns its socket, and
//! the distributed search needs inbound reachability a proxy cannot give.

pub mod config;
pub mod mapping;
mod provider;
pub mod ranking;
mod worker;

pub use config::{
    DEFAULT_LISTEN_PORT, DEFAULT_SEARCH_TIMEOUT, DEFAULT_SERVER, DEFAULT_SERVER_PORT,
    SoulSeekConfig,
};
pub use mapping::candidates_from;
pub use provider::SoulSeekProvider;
pub use ranking::{compare_sources, rank_sources, rank_sources_for_query};
pub use worker::DownloadProgress;
