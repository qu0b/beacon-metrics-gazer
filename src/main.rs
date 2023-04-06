use crate::config::fetch_genesis;
use crate::ranges::parse_ranges;
use crate::util::resolve_path_or_url;
use anyhow::{anyhow, Context, Result};
use clap::Parser;
use config::{fetch_config, ConfigSpec};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use metrics::{set_gauge, TARGET_PARTICIPATION};
use prettytable::{format, Cell, Row, Table};
use prometheus::{Encoder, TextEncoder};
use ssz_state::{deserialize_partial_state, StatePartial};
use std::convert::Infallible;
use std::io::prelude::*;
use std::net::SocketAddr;
use std::ops::Range;
use std::time::Duration;
use tokio::time;

//use ssz_state::parse_epoch_participation;
//use ssz_state::ConfigSpec;

mod config;
mod metrics;
mod ranges;
mod ssz_state;
mod util;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Beacon HTTP API URL: http://1.2.3.4:4000
    url: String,
    #[arg(long)]
    /// Index ranges to group IDs as JSON or TXT. Example:
    /// `{"0..100": "lh-geth-0", "100..200": "lh-geth-1"}
    ranges: Option<String>,
    /// Local path or URL containing a file with index ranges
    /// with the format as defined in --ranges
    #[arg(long)]
    ranges_file: Option<String>,
    /// Dump participation ranges print to stderr on each fetch
    #[arg(long)]
    dump: bool,
}

type IndexRanges = Vec<(String, Range<usize>)>;
type ParticipationByRange = Vec<(String, Range<usize>, f32)>;

async fn handle_request(_req: Request<Body>) -> Result<Response<Body>, Infallible> {
    // Create the response
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = vec![];
    encoder.encode(&metric_families, &mut buffer).unwrap();

    Ok(Response::builder()
        .header("Content-Type", encoder.format_type())
        .body(Body::from(buffer))
        .unwrap())
}

async fn fetch_epoch_participation(
    config: &ConfigSpec,
    beacon_url: &str,
    // slot: u64,
) -> Result<StatePartial> {
    let req = reqwest::Client::new()
        .get(format!("{beacon_url}/eth/v2/debug/beacon/states/head",))
        .header(reqwest::header::ACCEPT, "application/octet-stream")
        .send()
        .await?;
    let state_buf = req.bytes().await?;

    let mut f = std::fs::File::create("state.ssz").unwrap();
    f.write_all(&state_buf).unwrap();

    Ok(deserialize_partial_state(config, &state_buf)?)
}

// https://github.com/ethereum/consensus-specs/blob/4a27f855439c16612ab1ae3995d71bed54f979ea/specs/altair/beacon-chain.md#participation-flag-indices
// const TIMELY_SOURCE_FLAG_INDEX: u8 = 0;
const TIMELY_TARGET_FLAG_INDEX: u8 = 1;
// const TIMELY_HEAD_FLAG_INDEX: u8 = 2;
// const TIMELY_SOURCE: u8 = 1 << TIMELY_SOURCE_FLAG_INDEX;
const TIMELY_TARGET: u8 = 1 << TIMELY_TARGET_FLAG_INDEX;
// const TIMELY_HEAD: u8 = 1 << TIMELY_HEAD_FLAG_INDEX;

fn has_flag(flag: u8, mask: u8) -> bool {
    flag & mask == mask
}

fn group_target_participation(ranges: &IndexRanges, state: &StatePartial) -> ParticipationByRange {
    ranges
        .iter()
        .map(|(range_name, range)| {
            let target_count: u32 = state.previous_epoch_participation[range.clone()]
                .iter()
                .map(|f| has_flag(*f, TIMELY_TARGET) as u32)
                .sum();
            let target_ratio = target_count as f32 / (range.end - range.start) as f32;
            (range_name.clone(), range.clone(), target_ratio)
        })
        .collect()
}

fn set_participation_to_metrics(participation_by_range: &ParticipationByRange) {
    for (range_name, _, target_ratio) in participation_by_range.iter() {
        set_gauge(&TARGET_PARTICIPATION, &[range_name], *target_ratio as f64);
    }
}

fn dump_participation_to_stdout(participation_by_range: &ParticipationByRange) {
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);

    table.add_row(Row::new(vec![
        Cell::new("Name"),
        Cell::new("Range"),
        Cell::new("Target participation"),
    ]));

    for (range_name, range, target_ratio) in participation_by_range.iter() {
        table.add_row(Row::new(vec![
            Cell::new(&range_name),
            Cell::new(&format!("{:?}", &range)),
            Cell::new(&target_ratio.to_string()),
        ]));
    }

    table.printstd();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let beacon_url = cli.url;

    // Parse groups file mapping index ranges to host names
    let ranges_str = if let Some(ranges_str) = cli.ranges {
        ranges_str
    } else if let Some(path_or_url) = cli.ranges_file {
        resolve_path_or_url(&path_or_url).await?
    } else {
        return Err(anyhow!("Must set --groups or --groups_file"));
    };
    let ranges = parse_ranges(&ranges_str)?;

    println!("connecting to beacon URL {:?}", beacon_url);

    let genesis = fetch_genesis(&beacon_url).await.context("fetch_genesis")?;
    println!("beacon genesis {:?}", genesis);

    let config = fetch_config(&beacon_url).await.context("fetch_config")?;
    println!("beacon config {:?}", config);

    // Background task fetching state every interval and registering participation
    // in metrics with provided index ranges
    tokio::spawn(async move {
        loop {
            match fetch_epoch_participation(&config, &beacon_url).await {
                Ok(state) => {
                    let participation_by_range = group_target_participation(&ranges, &state);
                    set_participation_to_metrics(&participation_by_range);
                    if cli.dump {
                        dump_participation_to_stdout(&participation_by_range);
                    }
                }
                Err(e) => eprintln!("error fetching state: {:?}", e),
            };

            // Run once on boot, then every interval at end of epoch
            time::sleep(Duration::from_secs(5)).await;
        }
    });

    // Start metrics server
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let make_svc =
        make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle_request)) });
    let server = Server::bind(&addr).serve(make_svc);

    println!("Server is running on http://{}", addr);
    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }

    Ok(())
}