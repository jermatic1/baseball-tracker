use std::path::PathBuf;

use crate::convert::{print_hit, to_estimate};

pub async fn live(path: PathBuf, replay: bool) -> Result<(), String> {
    if !replay {
        #[cfg(not(feature = "oak"))]
        {
            eprintln!("rebuild with --features oak");
            std::process::exit(1);
        }
        #[cfg(feature = "oak")]
        {
            let _ = path;
            return Err("live camera loop is not implemented; pass --replay".into());
        }
    }
    replay_hits(path).await
}

async fn replay_hits(path: PathBuf) -> Result<(), String> {
    let session = tracker::Session::open(&path).map_err(|e| e.to_string())?;
    let mut hits = tracker::recompute_hits(&session).map_err(|e| e.to_string())?;
    session.write_hits(&hits).map_err(|e| e.to_string())?;
    let url = session.config.simulator.launch_url.to_string();
    let client = reqwest::Client::new();
    for i in 0..hits.len() {
        print_hit(&hits[i]);
        if hits[i].posted || hits[i].kind != tracker::EventKind::Hit {
            continue;
        }
        if !hits[i].confident {
            eprintln!("{}: not confident", hits[i].clip);
            continue;
        }
        let est = to_estimate(&hits[i]);
        if let Err(reason) = tracker::postable(&est) {
            eprintln!("{}: {reason}", hits[i].clip);
            continue;
        }
        // The full measured record is the contract; the simulator predicts
        // flight, distance, and what-ifs from it.
        let body = serde_json::to_value(&hits[i]).map_err(|e| e.to_string())?;
        let resp = match client.post(&url).json(&body).send().await {
            Ok(resp) => resp,
            Err(e) => {
                eprintln!("{}: simulator unreachable: {e}", hits[i].clip);
                continue;
            }
        };
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        match status {
            200 => {
                hits[i].posted = true;
                session.write_hits(&hits).map_err(|e| e.to_string())?;
            }
            409 => eprintln!("ball already in flight"),
            400 | 503 => eprintln!("{text}"),
            other => eprintln!("{other} {text}"),
        }
    }
    Ok(())
}
