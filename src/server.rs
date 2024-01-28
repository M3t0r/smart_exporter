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
        self.http_server.await
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

    let exposition = collector.clone().collect().await.map_err(HttpError::for_internal_error)?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/openmetrics-text; version=1.0.0; charset=utf-8")
        .body(
            exposition
            .into(),
        )?
    )
}
