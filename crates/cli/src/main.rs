mod aim;
mod calibrate;
mod capture;
mod convert;
#[cfg(feature = "detect")]
mod detect;
#[cfg(feature = "detect")]
mod detector;
mod discovery;
mod error;
mod history;
mod hub;
mod jpeg;
mod label;
mod label_tracks;
mod mock;
mod protocol;
#[cfg(feature = "detect")]
mod replay;
mod review;
mod sessions;
#[cfg(feature = "detect")]
mod standby;
mod train;
mod users;
#[cfg(feature = "detect")]
mod watch;
mod web;

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use calibrate::calibrate;
use capture::capture;
use convert::print_estimate;
use label::label;
use review::review;

/// The model the tracker loads and the Train page replaces.
const DEFAULT_MODEL: &str = "models/detector.onnx";
const DEFAULT_BIND: &str = "0.0.0.0:7880";

#[derive(Parser)]
#[command(name = "tracker")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Capture {
        data: PathBuf,
        #[arg(long)]
        demo: bool,
        #[arg(long)]
        once: bool,
    },
    /// Solve the camera pose from the home plate and write it to the data directory.
    Calibrate {
        data: PathBuf,
    },
    Review {
        data: PathBuf,
        #[arg(long, default_value = "127.0.0.1:7879")]
        bind: SocketAddr,
    },
    Label {
        data: PathBuf,
        #[arg(long, default_value = "0.0.0.0:7881")]
        bind: SocketAddr,
    },
    Synth {
        data: PathBuf,
    },
    Detect {
        data: PathBuf,
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,
        #[arg(long)]
        clip: Option<String>,
    },
    /// Serve the socket with no camera, making up a pitch or hit every
    /// few seconds, for working on simulators and pages anywhere.
    Mock {
        data: PathBuf,
        /// Seconds between made-up events.
        #[arg(long, default_value_t = 4.0)]
        period: f64,
        /// Where the Train page promotes a new model to.
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: PathBuf,
        #[arg(long, default_value = DEFAULT_BIND)]
        bind: SocketAddr,
    },
    /// Track live: serve the socket, and while a client is
    /// connected find the ball in every frame, measure each hit or throw,
    /// broadcast it, and save the clip.
    Watch {
        data: PathBuf,
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,
        /// Replay recorded clips instead of opening the camera.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Seconds of silence between replayed clips.
        #[arg(long, default_value_t = 1.0)]
        gap: f64,
        /// Address for the pages and the socket; a replay can take another
        /// port beside the live tracker.
        #[arg(long, default_value = DEFAULT_BIND)]
        bind: SocketAddr,
    },
}

#[tokio::main]
async fn main() {
    let code = match run().await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    };
    // The NVIDIA driver on Jetson aborts in its library destructors, so skip them.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    unsafe { libc::_exit(code) }
}

async fn run() -> Result<(), String> {
    match Cli::parse().command {
        Command::Capture { data, demo, once } => capture(data, demo, once).await,
        Command::Calibrate { data } => calibrate(data),
        Command::Review { data, bind } => review(data, bind).await,
        Command::Label { data, bind } => label(data, bind).await,
        Command::Synth { data } => synth(data),
        Command::Detect { data, model, clip } => detect(data, model, clip).await,
        Command::Mock {
            data,
            period,
            model,
            bind,
        } => mock::mock(data, period, model, bind).await,
        Command::Watch {
            data,
            model,
            from,
            gap,
            bind,
        } => watch(data, model, from, gap, bind).await,
    }
}

async fn watch(
    dir: PathBuf,
    model: String,
    from: Option<PathBuf>,
    gap: f64,
    bind: SocketAddr,
) -> Result<(), String> {
    #[cfg(feature = "detect")]
    {
        watch::watch(dir, model, from, gap, bind).await
    }
    #[cfg(not(feature = "detect"))]
    {
        let _ = (dir, model, from, gap, bind);
        Err("rebuild with --features detect".into())
    }
}

async fn detect(dir: PathBuf, model: String, clip: Option<String>) -> Result<(), String> {
    #[cfg(feature = "detect")]
    {
        detect::detect(dir, model, clip).await
    }
    #[cfg(not(feature = "detect"))]
    {
        let _ = (dir, model, clip);
        Err("rebuild with --features detect".into())
    }
}

fn synth(dir: PathBuf) -> Result<(), String> {
    let est = tracker::write_synth(&dir).map_err(|e| e.to_string())?;
    print_estimate(&est);
    Ok(())
}
