pub use env_logger::filter;

pub use level::DataDogLogLevel;
pub use logger::DataDogLogger;

pub use self::log::DataDogLog;

mod blocking;
mod level;
mod log;
mod logger;
#[cfg(feature = "nonblocking")]
mod nonblocking;
