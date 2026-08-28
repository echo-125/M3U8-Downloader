mod buffer;
mod rolling;

pub use buffer::{LogBuffer, LogLevel};
pub use rolling::init;

pub const MAX_GUI_ENTRIES: usize = 500;
