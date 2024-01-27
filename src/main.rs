use std::path::PathBuf;

use slog::{Drain, o};
use clap::{Parser, ValueEnum};

pub mod smartctl;
pub mod server;
pub mod collector;

use crate::smartctl::SmartctlInvoker;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum InvokerChoice {
    /// call smartctl directly
    Standard,
    /// call smartctl via sudo so smart_exporter doesn't have to run as root
    Sudo,
    /// use smartctl output saved to files insteadd of calling smartctl directly
    File,
}

/// Export S.M.A.R.T. drive stats in Prometheus format
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct CLIOptions {
    /// how to invoke smartctl
    #[arg(short, long, default_value = "sudo")]
    invoker: InvokerChoice,

    /// in which path to look for files for the file invoker
    #[arg(long, default_value = "./")]
    file_invoker_path: PathBuf,

    #[arg(short = 'b', long = "bind", default_value = "0.0.0.0:9940")]
    bind_address: std::net::SocketAddr,
}

fn make_logger() -> slog::Logger {
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::CompactFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();

    slog::Logger::root(drain, o!())
}

#[tokio::main]
async fn main() {
    let args = CLIOptions::parse();

    let log = make_logger();
    //let mut invoker = smartctl::FileInvoker::new(PathBuf::new("./tests/simple/"));
    let mut invoker = smartctl::SudoInvoker{};

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
        args.bind_address,
        &log
    ).expect("failed to configure server");

    server.run().await.expect("failed to start server");
    collector_worker.await.expect("collector worker error");
}

#[cfg(test)]
mod test {
    use crate::CLIOptions;
    use clap::CommandFactory;

    #[test]
    fn verify_cli() {
        CLIOptions::command().debug_assert();
    }
}
