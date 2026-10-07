use std::path::PathBuf;

use crate::convert::{print_hit, to_estimate};

pub async fn live(path: PathBuf, replay: bool) -> Result<(), String> {
    if !replay {
        return Err(
            "pass --replay to recompute and post saved clips; `tracker watch` runs a live session"
                .into(),
        );
    }
    let session = tracker::Session::open(&path).map_err(|e| e.to_string())?;
    let mut hits = tracker::recompute_hits(&session).map_err(|e| e.to_string())?;
    for hit in &hits {
        print_hit(hit);
    }
    post_hits(&session, &mut hits, &reqwest::Client::new()).await
}

/// Post every confident, unposted hit to the simulator and mark it posted in
/// the session's hit list as each one is accepted.
pub async fn post_hits(
    session: &tracker::Session,
    hits: &mut [tracker::HitRecord],
    client: &reqwest::Client,
) -> Result<(), String> {
    let url = session.config.simulator.launch_url.as_str();
    for i in 0..hits.len() {
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
        let resp = match client.post(url).json(&body).send().await {
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
                session.write_hits(hits).map_err(|e| e.to_string())?;
                println!("{}: posted", hits[i].clip);
            }
            409 => eprintln!("ball already in flight"),
            400 | 503 => eprintln!("{text}"),
            other => eprintln!("{other} {text}"),
        }
    }
    Ok(())
}
