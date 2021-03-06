// © 2019 3D Robotics. License: Apache-2.0
use hyper;
use rusoto_s3;
use rusoto_core;
use log;
use env_logger;
use log_panics;

mod stream_range;
mod serve_range;
mod zip;
mod upstream;
mod s3url;

use std::sync::Arc;
use std::convert::Infallible;

use clap::{Arg, App};
use hyper::{ Client, Request, Response, Body, Server, StatusCode, client::HttpConnector };
use hyper::service::{ make_service_fn, service_fn };
use hyper_tls::HttpsConnector;

type HyperClient = Client<HttpsConnector<HttpConnector>>;
type S3Arc = Arc<dyn rusoto_s3::S3 + Send + Sync>;

#[derive(Clone)]
pub struct Config {
    upstream: String,
    strip_prefix: String,
    via_zip_stream_header_value: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut logger = env_logger::Builder::from_default_env();
    logger.filter_level(log::LevelFilter::Info);
    logger.write_style(env_logger::WriteStyle::Never);
    logger.init();
    log_panics::init();
    log::info!("Startup");

    let matches = App::new("zipstream")
        .arg(Arg::with_name("upstream")
            .long("upstream")
            .takes_value(true)
            .help("Upstream server that provides zip file manifests")
            .value_name("URL")
            .required(true))
        .arg(Arg::with_name("strip-prefix")
            .long("strip-prefix")
            .takes_value(true)
            .help("Remove a prefix from the URL path before proxying to upstream server")
            .default_value(""))
        .arg(Arg::with_name("header-value")
            .long("header-value")
            .takes_value(true)
            .help("Value passed in the X-Via-Zip-Stream header on the request to the upstream server")
            .default_value("true"))
        .arg(Arg::with_name("listen")
            .long("listen")
            .takes_value(true)
            .help("IP:port to listen for HTTP connections")
            .default_value("127.0.0.1:3000"))
        .get_matches();

    let region = rusoto_core::Region::default();
    let s3_client = Arc::new(rusoto_s3::S3Client::new(region)) as S3Arc;

    let config = Config {
        upstream: matches.value_of("upstream").unwrap().into(),
        strip_prefix:matches.value_of("strip-prefix").unwrap().into(),
        via_zip_stream_header_value: matches.value_of("header-value").unwrap().into(),
    };

    let client = Client::builder().build::<_, hyper::Body>(HttpsConnector::new());

    let addr = matches.value_of("listen").unwrap().parse().expect("invalid `listen` value");

    let new_svc = make_service_fn(move |_conn| {
        let client = client.clone();
        let s3_client = s3_client.clone();
        let config = config.clone();

        async {
            Ok::<_, Infallible>(service_fn(move |req| {
                let client = client.clone();
                let s3_client = s3_client.clone();
                let config = config.clone();

                async move {
                    Ok::<_, Infallible>(match handle_request(req, &client, &s3_client, &config).await {
                        Ok(response) => response,
                        Err((status, message)) => Response::builder().status(status).body(message.into()).unwrap(),
                    })
                }
            }))
        }
    });

    Server::bind(&addr).serve(new_svc).await?;

    Ok(())
}

async fn handle_request(req: Request<Body>, client: &HyperClient, s3_client: &S3Arc, config: &Config) -> Result<Response<Body>, (StatusCode, &'static str)> {
    log::info!("Request: {} {}", req.method(), req.uri());
    let upstream_req = upstream::request(&config, &req)?;
    let upstream_res = client.request(upstream_req).await.map_err(|e| {
        log::error!("Failed to connect upstream: {}", e);
        (StatusCode::SERVICE_UNAVAILABLE, "Upstream connection failed")
    })?;

    if upstream_res.headers().get("X-Zip-Stream").is_some() {
        let body = hyper::body::to_bytes(upstream_res.into_body()).await.map_err(|e| {
            log::error!("Failed to read upstream body: {}", e);
            (StatusCode::SERVICE_UNAVAILABLE, "Upstream request failed")
        })?;

        upstream::response(s3_client, &req, &body[..])
    } else {
        log::info!("Request proxied from upstream");
        Ok(upstream_res)
    }
}
