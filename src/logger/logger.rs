use std::{fmt::Display, ops::Drop, thread};

use flume::{bounded, unbounded, Receiver, Sender};
#[cfg(feature = "nonblocking")]
use futures::Future;
use log::{Log, Metadata, Record};

#[cfg(feature = "nonblocking")]
use crate::client::AsyncDataDogClient;
use crate::logger::filter;
use crate::{client::DataDogClient, config::DataDogConfig, error::DataDogLoggerError};

use super::blocking;
#[cfg(feature = "nonblocking")]
use super::nonblocking;
use super::{level::DataDogLogLevel, log::DataDogLog};

#[derive(Debug)]
/// Logger that logs directly to DataDog via HTTP(S)
pub struct DataDogLogger {
    config: DataDogConfig,
    logsender: Option<Sender<DataDogLog>>,
    selflogrv: Option<Receiver<String>>,
    selflogsd: Option<Sender<String>>,
    logger_handle: Option<thread::JoinHandle<()>>,
    filter: filter::Filter,
}

impl DataDogLogger {
    /// Exposes self log of the logger.
    ///
    /// Contains diagnostic messages with details of errors occuring inside logger.
    /// It will be `None`, unless `enable_self_log` in [`DataDogConfig`](crate::config::DataDogConfig) is set to `true`.
    pub fn selflog(&self) -> &Option<Receiver<String>> {
        &self.selflogrv
    }

    /// Creates new blocking DataDogLogger instance
    ///
    /// What it means is that no executor is used to host DataDog network client. A new thread is started instead.
    /// It receives messages to log and sends them in batches in blocking fashion.
    /// As this is a separate thread, calling [`log`](Self::log) does not imply any IO operation, thus is quite fast.
    ///
    /// # Examples
    ///```rust
    ///use log::LevelFilter;
    /// use datadog_logs::{config::DataDogConfig, logger::{DataDogLogger, filter}, client::HttpDataDogClient};
    ///
    ///let config = DataDogConfig::default();
    ///let client = HttpDataDogClient::new(&config).unwrap();
    ///let filter = filter::Builder::new().filter_level(LevelFilter::Info).build();
    ///let logger = DataDogLogger::blocking(client, config, filter);
    ///```
    pub fn blocking<T>(client: T, config: DataDogConfig, filter: filter::Filter) -> Self
    where
        T: DataDogClient + Send + 'static,
    {
        let (slsender, slreceiver) = if config.enable_self_log {
            let (s, r) = bounded::<String>(100);
            (Some(s), Some(r))
        } else {
            (None, None)
        };
        let slogsender_clone = slsender.clone();
        let (sender, receiver) = match config.messages_channel_capacity {
            Some(capacity) => bounded(capacity),
            None => unbounded(),
        };

        let logger_handle =
            thread::spawn(move || blocking::logger_thread(client, receiver, slsender));

        DataDogLogger {
            config,
            logsender: Some(sender),
            selflogrv: slreceiver,
            selflogsd: slogsender_clone,
            logger_handle: Some(logger_handle),
            filter,
        }
    }

    /// Creates new non-blocking `DataDogLogger` instance
    ///
    /// Internally spawns logger future to `tokio` runtime.
    /// It is equivalent to calling [`non_blocking_cold`](Self::non_blocking_cold) and spawning future to Tokio runtime.
    /// Thus it is only a convinience function.
    #[cfg(feature = "with-tokio")]
    pub fn non_blocking_with_tokio<T>(
        client: T,
        config: DataDogConfig,
        filter: filter::Filter,
    ) -> Self
    where
        T: AsyncDataDogClient + Send + 'static,
    {
        let (logger, future) = Self::non_blocking_cold(client, config, filter);
        tokio::spawn(future);
        logger
    }

    /// Creates new non-blocking `DataDogLogger` instance
    ///
    /// What it means is that logger requires executor to run. This executor will host a task that will receive messages to log.
    /// It will log them using non blocking (asynchronous) implementation of network client.
    ///
    /// It returns a `Future` that needs to be spawned for logger to work. This `Future` is a task that is responsible for sending messages.
    /// Although a little inconvinient, it is completely executor agnostic.
    ///
    /// # Examples
    ///```rust
    ///use log::LevelFilter;
    ///use datadog_logs::{config::DataDogConfig, logger::{DataDogLogger, filter}, client::HttpDataDogClient};
    ///
    ///# async fn func() {
    ///let config = DataDogConfig::default();
    ///let client = HttpDataDogClient::new(&config).unwrap();
    ///let filter = filter::Builder::new().filter_level(LevelFilter::Info).build();
    ///let (logger, future) = DataDogLogger::non_blocking_cold(client, config, filter);
    ///
    ///tokio::spawn(future);
    ///# }
    ///```
    #[cfg(feature = "nonblocking")]
    pub fn non_blocking_cold<T>(
        client: T,
        config: DataDogConfig,
        filter: filter::Filter,
    ) -> (Self, impl Future<Output = ()>)
    where
        T: AsyncDataDogClient,
    {
        let (slsender, slreceiver) = if config.enable_self_log {
            let (s, r) = bounded::<String>(100);
            (Some(s), Some(r))
        } else {
            (None, None)
        };
        let slogsender_clone = slsender.clone();
        let (logsender, logreceiver) = match config.messages_channel_capacity {
            Some(capacity) => bounded(capacity),
            None => unbounded(),
        };
        let logger_future = nonblocking::logger_future(client, logreceiver, slsender);

        let logger = DataDogLogger {
            config,
            logsender: Some(logsender),
            selflogrv: slreceiver,
            selflogsd: slogsender_clone,
            logger_handle: None,
            filter,
        };

        (logger, logger_future)
    }

    /// Sends log to DataDog thread or task.
    ///
    /// This function does not invoke any IO operation by itself. Instead it sends messages to logger thread or task using channels.
    /// Therefore it is quite lightweight.
    ///
    /// ## Examples
    ///
    ///```rust
    ///use log::LevelFilter;
    ///use datadog_logs::{config::DataDogConfig, logger::{DataDogLogger, DataDogLogLevel, filter}, client::HttpDataDogClient};
    ///
    ///let config = DataDogConfig::default();
    ///let client = HttpDataDogClient::new(&config).unwrap();
    ///let filter = filter::Builder::new().filter_level(LevelFilter::Info).build();
    ///let logger = DataDogLogger::blocking(client, config, filter);
    ///
    ///logger.log("message", DataDogLogLevel::Error);
    ///```
    pub fn log<T: Display>(&self, message: T, level: DataDogLogLevel) {
        let log = DataDogLog {
            message: message.to_string(),
            ddtags: self.config.tags.clone(),
            service: self.config.service.clone().unwrap_or_default(),
            host: self.config.hostname.clone().unwrap_or_default(),
            ddsource: self.config.source.clone(),
            level: level.to_string(),
        };

        if let Some(ref sender) = self.logsender {
            match sender.try_send(log) {
                Ok(()) => {
                    // nothing
                }
                Err(e) => {
                    if let Some(ref selflog) = self.selflogsd {
                        selflog.try_send(e.to_string()).unwrap_or_default();
                    }
                }
            }
        }
    }

    /// Initializes blocking DataDogLogger with `log` crate.
    /// # Examples
    ///
    ///```rust
    ///use datadog_logs::{config::DataDogConfig, logger::{DataDogLogger, DataDogLogLevel, filter}, client::HttpDataDogClient};
    ///use log::*;
    ///
    ///let config = DataDogConfig::default();
    ///let client = HttpDataDogClient::new(&config).unwrap();
    ///let filter = filter::Builder::new().filter_level(LevelFilter::Info).build();
    ///
    ///DataDogLogger::set_blocking_logger(client, config, filter);
    ///
    ///error!("An error occurred");
    ///warn!("A warning")
    ///```
    pub fn set_blocking_logger<T>(
        client: T,
        config: DataDogConfig,
        filter: filter::Filter,
    ) -> Result<(), DataDogLoggerError>
    where
        T: DataDogClient + Send + 'static,
    {
        let level_filter = filter.filter();
        let logger = DataDogLogger::blocking(client, config, filter);
        log::set_boxed_logger(Box::new(logger))?;
        log::set_max_level(level_filter);
        Ok(())
    }

    /// Initializes nonblocking DataDogLogger with `log` crate.
    ///
    /// To make logger work, returned future has to be spawned to executor.
    /// # Examples
    ///```rust
    ///use datadog_logs::{config::DataDogConfig, logger::{DataDogLogger, filter}, client::HttpDataDogClient};
    ///use log::*;
    ///
    ///# async fn func() {
    ///let config = DataDogConfig::default();
    ///let client = HttpDataDogClient::new(&config).unwrap();
    ///let filter = filter::Builder::new().filter_level(LevelFilter::Info).build();
    ///let future = DataDogLogger::set_nonblocking_logger(client, config, filter).unwrap();
    ///
    ///tokio::spawn(future);
    ///
    ///error!("An error occured");
    ///warn!("A warning");
    ///# }
    ///```
    #[cfg(feature = "nonblocking")]
    pub fn set_nonblocking_logger<T>(
        client: T,
        config: DataDogConfig,
        filter: filter::Filter,
    ) -> Result<impl Future<Output = ()>, DataDogLoggerError>
    where
        T: AsyncDataDogClient + Send + 'static,
    {
        let level_filter = filter.filter();
        let (logger, future) = DataDogLogger::non_blocking_cold(client, config, filter);
        log::set_boxed_logger(Box::new(logger))?;
        log::set_max_level(level_filter);
        Ok(future)
    }
}

impl Log for DataDogLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.filter.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if self.filter.matches(record) {
            let level = match record.level() {
                log::Level::Error => DataDogLogLevel::Error,
                log::Level::Warn => DataDogLogLevel::Warning,
                log::Level::Info => DataDogLogLevel::Informational,
                log::Level::Debug | log::Level::Trace => DataDogLogLevel::Debug,
            };

            self.log(
                format!(
                    "[{}] {}",
                    record.module_path().unwrap_or_default(),
                    record.args()
                ),
                level,
            );
        }
    }

    fn flush(&self) {}
}

impl Drop for DataDogLogger {
    fn drop(&mut self) {
        // drop sender to allow logger thread to close
        std::mem::drop(self.logsender.take());

        // wait for logger thread to finish to ensure all messages are flushed
        if let Some(handle) = self.logger_handle.take() {
            handle.join().unwrap_or_default();
        }
    }
}
