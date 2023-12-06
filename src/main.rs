use slog::{Drain, o};
use slog_term;
use slog_async;

mod smartctl {
    use std::vec::Vec;
    use serde::{de::DeserializeOwned, Deserialize, Serialize};
    use serde_json;

    use std::process::Command;
    use std::ffi::OsStr;

    const SUPPORTED_JSON_FORMAT_VERSION: &[u8] = &[1, 0];

    pub fn call<I, S, O>(args: I) -> std::io::Result<(O, Version)>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        O: Serialize + DeserializeOwned,
    {
        let output = Command::new("sudo")
            .env_clear()
            .env("PATH", "/bin/:/sbin/:/usr/bin/:/usr/sbin/")
            .args(["smartctl", "--json"])
            .args(args)
            .output()?;

        let parsed: Output<O> = serde_json::from_slice(&output.stdout).expect("failed to parse smartctl output");

        if parsed.json_format_version != SUPPORTED_JSON_FORMAT_VERSION {
            eprintln!(
                "warning: unkown JSON output format from smartctl: {}",
                parsed
                    .json_format_version
                    .iter()
                    .map(|i|i.to_string())
                    .collect::<Vec<_>>()
                    .join(".")
            );
        }

        Ok((parsed.output, parsed.smartctl.version_details))
    }

    #[derive(Debug, Deserialize, Serialize)]
    pub struct Output<O> {
        json_format_version: Vec<u8>,
        smartctl: InvocationDetails,
        #[serde(flatten)]
        output: O,
    }

    #[derive(Debug, Deserialize, Serialize)]
    struct InvocationDetails {
        #[serde(flatten)]
        version_details: Version,
        argv: Vec<String>,
        exit_status: u8,
    }

    #[derive(Debug, Deserialize, Serialize)]
    pub struct Version {
        version: Vec<u8>,
        svn_revision: String,
        platform_info: String,
    }

    pub mod device_scan {
        use std::option::Option;
        use std::vec::Vec;
        use std::path::PathBuf;
        use serde::{Deserialize, Serialize};

        #[derive(Debug, Deserialize, Serialize)]
        pub struct Scan {
            pub devices: Vec<Device>,
        }

        #[derive(Debug, Deserialize, Serialize)]
        pub struct Device {
            pub name: PathBuf,
            pub info_name: String,
            pub r#type: Type,
            pub protocol: Protocol,
        }

        #[derive(Debug, Deserialize, Serialize)]
        #[serde(rename_all = "lowercase")] 
        pub enum Type {
            Sat,
        }

        #[derive(Debug, Deserialize, Serialize)]
        #[serde(rename_all = "UPPERCASE")] 
        pub enum Protocol {
            Ata,
        }
    }
}

mod collector {
    use std::path::PathBuf;
    use std::time::{Instant, Duration};

    use tokio::sync::mpsc;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use prometheus_client::registry::Registry;
    use prometheus_client::encoding::text::encode;
    use slog::{error, debug, o};

    use crate::smartctl;

    pub type WorkerChannel = mpsc::Sender<oneshot::Sender<Result<String, String>>>;
    pub type CollectResult = Result<String, String>;

    #[derive(Debug)]
    pub struct Collector {
        registry: Registry,
        last_read_version: Option<smartctl::Version>,
        device_rescan_interval: Duration,
        last_device_scan: Option<Instant>,
        devices: Vec<smartctl::device_scan::Device>,
    }

    impl Collector {
        pub fn new() -> Self {
            Self{
                registry: Registry::default(),
                last_read_version: None,
                device_rescan_interval: Duration::from_secs(30),
                last_device_scan: None,
                devices: vec![],
            }
        }

        pub async fn collect(&mut self, log: &slog::Logger) -> CollectResult {
            let now = Instant::now();
            if self.last_device_scan.is_none() || (now - self.last_device_scan.unwrap() > self.device_rescan_interval) {
                self.last_device_scan = Some(now);
                debug!(log, "re-scanning devices");

                let (scan, last_read_version): (smartctl::device_scan::Scan, _) =
                    smartctl::call(["--scan-open"])
                        .map_err(|e| format!("failed to run smartctl: {:?}", e))?;

                self.devices = scan.devices;
                self.last_read_version = Some(last_read_version);
            }

            let mut buffer = String::new();
            encode(&mut buffer, &self.registry)
                .map_err(|e| format!("failed to encode prometheus exposition: {:?}", e))?;
            Ok(buffer)
        }

        /// Consumes `self` and starts a [tokio worker task](tokio::spawn). The
        /// returned Client can communicate with the worker task over [`mpsc`]
        /// channels and request an encoded Prometheus exposition. This function
        /// also returns the tasks [`JoinHandle`] you should await.
        pub fn start_worker(mut self, log: &slog::Logger) -> (Client, JoinHandle<()>) {
            let log = log.new(o!("worker" => "collector"));

            let (tx, mut rx) = mpsc::channel::<oneshot::Sender<CollectResult>>(1);
            let handle = tokio::spawn(async move {
                while let Some(return_channel) = rx.recv().await {
                    debug!(log, "got collect request in worker task");
                    debug!(log, "{:?}", self);
                    let collect_result = self.collect(&log).await;
                    if let Err(e) = return_channel.send(collect_result) {
                        error!(log, "failed to send response to client"; "error" => format!("{:?}", e));
                    }
                }
            });

            return (Client{channel: tx}, handle);
        }
    }

    #[derive(Debug, Clone)]
    pub struct Client {
        channel: WorkerChannel,
    }

    /// Communicates with a [`Collector`] in a [tokio worker task](tokio::spawn)
    /// via [`mpsc`] channels
    impl Client {
        /// Calls [`collect()`](Collector::collect()) on the [`Collector`] in
        /// the connected task
        pub async fn collect(&self) -> CollectResult {
            let (resp_tx, resp_rx) = oneshot::channel();
            self.channel
                .clone()
                .send(resp_tx)
                .await
                .map_err(|e| format!("failed to communicate with collector worker (tx): {:?}", e))?;

            resp_rx
                .await
                .map_err(|e| format!("failed to communicate with collector worker (rx): {:?}", e))?
        }
    }

    #[derive(Debug)]
    pub struct Device {
        name: PathBuf,
    }

    impl Device {
        pub async fn collect(&mut self) -> Result<(), String> {
            Ok(())
        }
    }
}

mod server {
    use std::net::SocketAddr;

    use dropshot::{HttpServerStarter, HttpServer, ConfigDropshot, ApiDescription, endpoint, RequestContext, HttpError};
    use http::{Response, StatusCode};
    use hyper::Body;

    use crate::collector::Client as CollectorClient;

    pub struct Server {
        http_server: HttpServer<ServerContext>,
    }

    #[derive(Debug)]
    struct ServerContext {
        collector: CollectorClient,
    }

    impl Server {
        pub fn new(collector: CollectorClient, bind_address: SocketAddr, log: &slog::Logger) -> Result<Self, String> {
            let ctx = ServerContext{
                collector,
            };

            let mut api = ApiDescription::<ServerContext>::new();
            api.register(index_handler).unwrap();
            api.register(metrics_handler).unwrap();

            let http_server =
                HttpServerStarter::<ServerContext>::new(
                    &ConfigDropshot {
                        bind_address,
                        request_body_max_bytes: 0,
                        tls: None,
                    },
                    api,
                    ctx,
                    log,
                )
                .map_err(|error| format!("failed to start server: {}", error))?
                .start();

            Ok(Self{
                http_server,
            })
        }

        pub async fn run(self) -> Result<(),String>{
            Ok(self.http_server.await?)
        }
    }

    #[endpoint {
        method = GET,
        path = "/",
    }]
    async fn index_handler(
        _rqctx: RequestContext<ServerContext>,
    ) -> Result<Response<Body>, HttpError>
    {
        let name = env!("CARGO_CRATE_NAME");
        let version = env!("CARGO_PKG_VERSION");
        let authors = env!("CARGO_PKG_AUTHORS");
        let homepage = env!("CARGO_PKG_HOMEPAGE");

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(http::header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(
                format!(r#"
<h1><pre>{}</pre></h1>
<p>v{} by {}, <a href="{}">homepage</a></p>
<ul><li><a href="/metrics">/metrics</a></li></ul>"#,
                    name,
                    version,
                    authors,
                    homepage
                )
                .into(),
            )?
        )
    }

    #[endpoint {
        method = GET,
        path = "/metrics",
    }]
    async fn metrics_handler(
        rqctx: RequestContext<ServerContext>,
    ) -> Result<Response<Body>, HttpError>
    {
        let collector = &rqctx.context().collector;

        let exposition = collector.clone().collect().await.map_err(|e| HttpError::for_internal_error(e))?;

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(http::header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")
            .body(
                exposition
                .into(),
            )?
        )
    }
}

fn make_logger() -> slog::Logger {
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::CompactFormat::new(decorator).build().fuse();
    let drain = slog_async::Async::new(drain).build().fuse();

    slog::Logger::root(drain, o!())
}

#[tokio::main]
async fn main() {
    let log = make_logger();

    // run once early on to check:
    //  - sudo available and configured
    //  - smartctl installed with json support
    //  - disk access granted
    let (_, _): (smartctl::device_scan::Scan, _) = smartctl::call(["--scan-open"])
        .expect("failed to run smartctl");

    let mut collector = collector::Collector::new();
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
