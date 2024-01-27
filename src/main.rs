use std::path::PathBuf;

use slog::{Drain, o};
use clap::{Parser, ValueEnum};

pub mod smartctl;
pub mod server;
pub mod collector;

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

    // Repeating the same instructions here is ugly, but I can't Box the
    // Invoker because on of the Trait functions would have a generic return
    // type, and I can't Box the Collector because apparently you can't build
    // a vtable for Traits with async functions...
    // If you can solve this puzzle please let me know!
    let (collector_channel, collector_worker) = match args.invoker {
        InvokerChoice::Standard => {
            let invoker = smartctl::NormalInvoker{};
            let mut collector = collector::Collector::with(invoker);

            // run once early on to check:
            //  - sudo available and configured
            //  - smartctl installed with json support
            //  - disk access granted
            collector.collect(&log).await.expect("first S.M.A.R.T. collection failed");

            collector.start_worker(&log)
        },
        InvokerChoice::Sudo => {
            let invoker = smartctl::SudoInvoker{};
            let mut collector = collector::Collector::with(invoker);

            // run once early on to check:
            //  - sudo available and configured
            //  - smartctl installed with json support
            //  - disk access granted
            collector.collect(&log).await.expect("first S.M.A.R.T. collection failed");

            collector.start_worker(&log)
        },
        InvokerChoice::File => {
            let invoker = smartctl::FileInvoker::new(&args.file_invoker_path);
            let mut collector = collector::Collector::with(invoker);

            // run once early on to check:
            //  - sudo available and configured
            //  - smartctl installed with json support
            //  - disk access granted
            collector.collect(&log).await.expect("first S.M.A.R.T. collection failed");

            collector.start_worker(&log)
        },
    };

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
