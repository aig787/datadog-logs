use log::LevelFilter;

use datadog_logs::{
    client::HttpDataDogClient,
    config::DataDogConfig,
    logger::{filter, DataDogLogLevel, DataDogLogger},
};

#[test]
fn test_logger_stops_http() {
    let config = DataDogConfig::default();
    let client = HttpDataDogClient::new(&config).unwrap();
    let filter = filter::Builder::new()
        .filter_level(LevelFilter::Info)
        .build();
    let logger = DataDogLogger::blocking::<HttpDataDogClient>(client, config, filter);

    logger.log("message", DataDogLogLevel::Alert);

    // it should hang forever if logging loop does not break
    std::mem::drop(logger);
}

#[tokio::test]
async fn test_async_logger_stops_http() {
    let config = DataDogConfig::default();
    let client = HttpDataDogClient::new(&config).unwrap();
    let filter = filter::Builder::new()
        .filter_level(LevelFilter::Info)
        .build();
    let (logger, future) =
        DataDogLogger::non_blocking_cold::<HttpDataDogClient>(client, config, filter);

    tokio::spawn(future);

    logger.log("message", DataDogLogLevel::Alert);

    // it should hang forever if logging loop does not break
    std::mem::drop(logger);
}
