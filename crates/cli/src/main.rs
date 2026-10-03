mod capture;
mod convert;
#[cfg(feature = "detect")]
mod detect;
mod error;
mod jpeg;
mod label;
mod live;
mod review;

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use capture::capture;
use convert::print_estimate;
use label::label;
use live::live;
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
        session: PathBuf,
        #[arg(long)]
        demo: bool,
        #[arg(long)]
        once: bool,
    },
    Review {
        session: PathBuf,
        #[arg(long, default_value = "127.0.0.1:7879")]
        bind: SocketAddr,
    },
    Label {
        session: PathBuf,
        #[arg(long, default_value = "0.0.0.0:7881")]
        bind: SocketAddr,
    },
    Live {
        session: PathBuf,
        #[arg(long)]
        replay: bool,
    },
    Synth {
        session: PathBuf,
    },
    Detect {
        session: PathBuf,
        #[arg(long, default_value = "yolo26n.onnx")]
        model: String,
        #[arg(long)]
        clip: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    match Cli::parse().command {
        Command::Capture {
            session,
            demo,
            once,
        } => capture(session, demo, once).await,
        Command::Review { session, bind } => review(session, bind).await,
        Command::Label { session, bind } => label(session, bind).await,
        Command::Live { session, replay } => live(session, replay).await,
        Command::Synth { session } => synth(session),
        Command::Detect {
            session,
            model,
            clip,
        } => detect(session, model, clip).await,
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
