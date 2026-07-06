pub mod tool;
pub mod connect;

pub use tool::NamespacedMcpTool;
pub use connect::McpConnectError;
pub use connect::{connect_all, McpDiscoveryReport};
