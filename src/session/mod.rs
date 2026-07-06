//! Session persistence: serializing the transcript + agent-facing message
//! history + active connection/model/tier to disk, keyed by project +
//! timestamp, per spec section 7.

pub mod paths;
pub mod store;
pub mod types;

pub use store::{list_sessions, load_session, save_session};
pub use types::{SessionFile, SessionSummary, SESSION_FILE_VERSION};
