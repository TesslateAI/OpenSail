//! Release pack is an at-most-once journal. A supervisor resumes
//! `dispatched` rows; GET/list/inspect never do.

use std::time::Duration;

use crate::http::Platform;

pub async fn reconcile_due(platform: &Platform) {
    let Ok(due) = platform.releases.list_dispatched().await else {
        return;
    };
    for release in due {
        platform.resume_dispatched_release(&release).await;
    }
}

pub fn spawn_loop(platform: Platform) {
    tokio::spawn(async move {
        loop {
            reconcile_due(&platform).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}
