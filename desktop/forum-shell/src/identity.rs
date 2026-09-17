//! Build identity is supplied from profiles/ai-vision-forum/product.json.
//! Runtime event profiles cannot change application identity or data roots.

pub const PRODUCT_NAME: &str = env!("FORUM_AGENT_PRODUCT_NAME");
pub const DATA_DIRECTORY_NAME: &str = env!("FORUM_AGENT_DATA_DIRECTORY_NAME");
