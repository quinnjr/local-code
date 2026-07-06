pub mod tool;
pub mod connect;

pub use tool::NamespacedMcpTool;
pub use connect::McpConnectError;
// TODO(task 5): re-enable once Task 5 lands
// pub use connect::{connect_all, McpDiscoveryReport};
