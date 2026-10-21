mod calibrate;
mod capture;
mod convert;
#[cfg(feature = "detect")]
mod detect;
#[cfg(feature = "detect")]
mod detector;
mod discovery;
mod error;
mod hub;
mod jpeg;
mod label;
mod protocol;
#[cfg(feature = "detect")]
mod replay;
mod review;
#[cfg(feature = "detect")]
mod watch;

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use calibrate::calibrate;
use capture::capture;
use convert::print_estimate;
use label::label;
use review::review;

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
        #[arg(long, default_value = "models/ball.onnx")]
        model: String,
        #[arg(long)]
        clip: Option<String>,
    },
    /// Track live: serve the socket, and while a client is
    /// connected find the ball in every frame, measure each hit or throw,
    /// broadcast it, and save the clip.
    Watch {
        data: PathBuf,
        #[arg(long, default_value = "models/ball.onnx")]
        model: String,
        /// Replay recorded clips instead of opening the camera.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Seconds of silence between replayed clips.
        #[arg(long, default_value_t = 1.0)]
        gap: f64,
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
        Command::Watch {
            data,
            model,
            from,
            gap,
        } => watch(data, model, from, gap).await,
    }
}

async fn watch(dir: PathBuf, model: String, from: Option<PathBuf>, gap: f64) -> Result<(), String> {
    #[cfg(feature = "detect")]
    {
        watch::watch(dir, model, from, gap).await
    }
    #[cfg(not(feature = "detect"))]
    {
        let _ = (dir, model, from, gap);
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
