use std::path::Path;

use slog::{Drain, o};
use slog_term;
use slog_async;

pub mod smartctl;
pub mod server;
pub mod collector;

use crate::smartctl::SmartctlInvoker;

fn make_logger() -> slog::Logger {
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::CompactFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();

    slog::Logger::root(drain, o!())
}

#[tokio::main]
async fn main() {
    let log = make_logger();
    let mut invoker = smartctl::FileInvoker::new(Path::new("./tests/simple/"));
    //let mut invoker = smartctl::SudoInvoker{};

    // run once early on to check:
    //  - sudo available and configured
    //  - smartctl installed with json support
    //  - disk access granted
    let (_, _): (smartctl::scan::Scan, _) = invoker.call(&log, ["--scan-open"])
        .expect("initial run of smartctl failed");

    let mut collector = collector::Collector::with(invoker);
    collector.collect(&log).await.expect("first S.M.A.R.T. collection failed");

    let (collector_channel, collector_worker) = collector.start_worker(&log);

    let server = server::Server::new(
        collector_channel,
        "0.0.0.0:9940".parse().unwrap(),
        &log
    ).expect("failed to configure server");

    server.run().await.expect("failed to start server");
    collector_worker.await.expect("collector worker error");
}
