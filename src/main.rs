use std::time::{Duration, Instant};

use clap::Parser;
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use rand::Rng as _;

// ---------------------------------------------------------------------------
// Cloudflare speed test endpoints (public, no auth required)
//   download: https://speed.cloudflare.com/__down?bytes=N
//   upload:   POST https://speed.cloudflare.com/__up  (body = raw bytes)
// ---------------------------------------------------------------------------

const CF_DOWN: &str = "https://speed.cloudflare.com/__down";
const CF_UP: &str = "https://speed.cloudflare.com/__up";

/// Minimal bandwidth test — download & upload via Cloudflare
#[derive(Parser)]
#[command(name = "speedtest-rs", version, about)]
struct Cli {
    /// Download payload size in MB
    #[arg(short, long, default_value_t = 100)]
    download_mb: u64,

    /// Upload payload size in MB
    #[arg(short, long, default_value_t = 25)]
    upload_mb: u64,

    /// Number of test rounds (results are averaged)
    #[arg(short, long, default_value_t = 3)]
    rounds: u32,

    /// Skip download test
    #[arg(long, default_value_t = false)]
    no_download: bool,

    /// Skip upload test
    #[arg(long, default_value_t = false)]
    no_upload: bool,
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn format_speed(bytes: u64, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs == 0.0 {
        return "∞".to_string();
    }
    let bits_per_sec = (bytes as f64 * 8.0) / secs;
    if bits_per_sec >= 1_000_000_000.0 {
        format!("{:.2} Gbps", bits_per_sec / 1_000_000_000.0)
    } else if bits_per_sec >= 1_000_000.0 {
        format!("{:.2} Mbps", bits_per_sec / 1_000_000.0)
    } else if bits_per_sec >= 1_000.0 {
        format!("{:.2} Kbps", bits_per_sec / 1_000.0)
    } else {
        format!("{:.0} bps", bits_per_sec)
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{} B", bytes)
    }
}

// ---------------------------------------------------------------------------
// Progress bar
// ---------------------------------------------------------------------------

fn make_progress_bar(total: u64, label: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(&format!(
                "  {{spinner:.cyan}} {label}  [{{bar:30.white/dim}}]  {{bytes}}/{{total_bytes}}  {{bytes_per_sec}}"
            ))
            .expect("valid template")
            .progress_chars("━╸─"),
    );
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

// ---------------------------------------------------------------------------
// Latency
// ---------------------------------------------------------------------------

async fn measure_latency(client: &reqwest::Client) -> Result<Duration, reqwest::Error> {
    // Small 1-byte download to measure RTT
    let start = Instant::now();
    let _resp = client
        .get(format!("{CF_DOWN}?bytes=1"))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    Ok(start.elapsed())
}

// ---------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------

/// Maximum single request size accepted by Cloudflare's __down endpoint.
/// Requests for >= 100_000_000 bytes get a 403, so we cap each chunk below that.
const CF_DOWN_MAX: u64 = 99_000_000;

async fn download_test(
    client: &reqwest::Client,
    size_bytes: u64,
) -> Result<(u64, Duration), Box<dyn std::error::Error>> {
    let pb = make_progress_bar(size_bytes, "↓ download");

    let start = Instant::now();
    let mut total: u64 = 0;
    let mut remaining = size_bytes;

    while remaining > 0 {
        let chunk_size = remaining.min(CF_DOWN_MAX);
        let url = format!("{CF_DOWN}?bytes={chunk_size}");
        let resp = client.get(&url).send().await?.error_for_status()?;
        let mut stream = resp.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            total += chunk.len() as u64;
            pb.set_position(total.min(size_bytes));
        }

        remaining = remaining.saturating_sub(chunk_size);
    }

    let elapsed = start.elapsed();
    pb.finish_and_clear();
    Ok((total, elapsed))
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

async fn upload_test(
    client: &reqwest::Client,
    size_bytes: u64,
) -> Result<(u64, Duration), Box<dyn std::error::Error>> {
    // Generate random payload
    let mut payload = vec![0u8; size_bytes as usize];
    rand::rng().fill_bytes(&mut payload);

    let pb = make_progress_bar(size_bytes, "↑ upload  ");

    let start = Instant::now();
    // Cloudflare __up accepts raw POST body
    let _resp = client
        .post(CF_UP)
        .header("Content-Type", "application/octet-stream")
        .body(payload)
        .send()
        .await?
        .error_for_status()?;

    let elapsed = start.elapsed();
    pb.set_position(size_bytes);
    pb.finish_and_clear();
    Ok((size_bytes, elapsed))
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    println!();
    println!("  speedtest-rs — bandwidth test via Cloudflare");
    println!("  ──────────────────────────────────");

    // -- Latency --
    let mut latencies = Vec::new();
    for _ in 0..5 {
        match measure_latency(&client).await {
            Ok(d) => latencies.push(d),
            Err(e) => {
                eprintln!("  ✗ latency probe failed: {e}");
                break;
            }
        }
    }
    if !latencies.is_empty() {
        let avg = latencies.iter().sum::<Duration>() / latencies.len() as u32;
        let min = latencies.iter().min().unwrap();
        println!(
            "  ping          {:.1} ms  (min {:.1} ms)",
            avg.as_secs_f64() * 1000.0,
            min.as_secs_f64() * 1000.0,
        );
    }

    // -- Download --
    if !cli.no_download {
        let size = cli.download_mb * 1_000_000;
        let mut speeds: Vec<f64> = Vec::new();

        for round in 0..cli.rounds {
            match download_test(&client, size).await {
                Ok((bytes, elapsed)) => {
                    let bps = (bytes as f64 * 8.0) / elapsed.as_secs_f64();
                    if cli.rounds > 1 {
                        println!(
                            "  ↓ round {}/{}   {:.2} Mbps",
                            round + 1,
                            cli.rounds,
                            bps / 1_000_000.0
                        );
                    }
                    speeds.push(bps);
                }
                Err(e) => eprintln!("  ✗ download round {} failed: {e}", round + 1),
            }
        }

        if !speeds.is_empty() {
            let avg_duration = Duration::from_secs_f64(
                (size as f64 * 8.0 * speeds.len() as f64) / speeds.iter().sum::<f64>(),
            );
            println!(
                "  ↓ download    {}  ({})",
                format_speed(size, avg_duration),
                format_size(size),
            );
            if speeds.len() > 1 {
                let max = speeds.iter().cloned().fold(f64::MIN, f64::max);
                let min = speeds.iter().cloned().fold(f64::MAX, f64::min);
                println!(
                    "                best {:.2} Mbps / worst {:.2} Mbps",
                    max / 1_000_000.0,
                    min / 1_000_000.0,
                );
                println!();
            }
        }
    }

    // -- Upload --
    if !cli.no_upload {
        let size = cli.upload_mb * 1_000_000;
        let mut speeds: Vec<f64> = Vec::new();

        for round in 0..cli.rounds {
            match upload_test(&client, size).await {
                Ok((bytes, elapsed)) => {
                    let bps = (bytes as f64 * 8.0) / elapsed.as_secs_f64();
                    if cli.rounds > 1 {
                        println!(
                            "  ↑ round {}/{}   {:.2} Mbps",
                            round + 1,
                            cli.rounds,
                            bps / 1_000_000.0
                        );
                    }
                    speeds.push(bps);
                }
                Err(e) => eprintln!("  ✗ upload round {} failed: {e}", round + 1),
            }
        }

        if !speeds.is_empty() {
            let avg_duration = Duration::from_secs_f64(
                (size as f64 * 8.0 * speeds.len() as f64) / speeds.iter().sum::<f64>(),
            );
            println!(
                "  ↑ upload      {}  ({})",
                format_speed(size, avg_duration),
                format_size(size),
            );
            if speeds.len() > 1 {
                let max = speeds.iter().cloned().fold(f64::MIN, f64::max);
                let min = speeds.iter().cloned().fold(f64::MAX, f64::min);
                println!(
                    "                best {:.2} Mbps / worst {:.2} Mbps",
                    max / 1_000_000.0,
                    min / 1_000_000.0,
                );
            }
        }
    }

    println!();
    Ok(())
}
