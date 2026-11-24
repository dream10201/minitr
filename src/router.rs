use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use reqwest::Client;

use crate::settings::Profile;
use crate::translate::{self, Translation};

const PROBE_FIRST_DELAY: Duration = Duration::from_secs(60);
const PROBE_MAX_DELAY: Duration = Duration::from_secs(60 * 60);
const PROBE_CYCLE: Duration = Duration::from_secs(24 * 60 * 60);

/// Fails over between profiles in the user's order. A failing profile is benched
/// until a background probe sees it answer again.
pub struct Router {
    client: Client,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    profiles: Arc<[Profile]>,
    benched: HashSet<Profile>,
    probing: HashSet<Profile>,
}

impl Router {
    pub fn new(client: Client, profiles: &[Profile]) -> Arc<Self> {
        let router = Arc::new(Self {
            client,
            state: Mutex::default(),
        });
        router.set_profiles(profiles);
        router
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn set_profiles(&self, profiles: &[Profile]) {
        let complete: Arc<[Profile]> = profiles
            .iter()
            .filter(|p| p.is_complete())
            .cloned()
            .collect();
        let mut state = self.lock();
        state.benched.retain(|p| complete.contains(p));
        state.profiles = complete;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Must run inside the Tokio runtime: failures spawn probe tasks on it.
    pub async fn translate(
        self: &Arc<Self>,
        text: &str,
        mut on_progress: impl FnMut(&str) + Send,
    ) -> Result<Translation> {
        let candidates: Vec<Profile> = {
            let state = self.lock();
            let healthy: Vec<_> = state
                .profiles
                .iter()
                .filter(|p| !state.benched.contains(p))
                .cloned()
                .collect();
            // When everything is benched, still try them all rather than refuse.
            if healthy.is_empty() {
                state.profiles.to_vec()
            } else {
                healthy
            }
        };
        if candidates.is_empty() {
            bail!("没有可用的接口配置。");
        }

        let mut last_error = None;
        for profile in &candidates {
            match translate::translate(&self.client, text, profile, &mut on_progress).await {
                Ok(translation) => {
                    self.lock().benched.remove(profile);
                    return Ok(translation);
                }
                Err(error) => {
                    on_progress("");
                    self.bench(profile);
                    last_error = Some(error);
                }
            }
        }
        let error = last_error.expect("at least one candidate was tried");
        Err(match candidates.len() {
            1 => error,
            n => anyhow!("{n} 个接口均不可用，最后错误：{error:#}"),
        })
    }

    fn bench(self: &Arc<Self>, profile: &Profile) {
        let mut state = self.lock();
        state.benched.insert(profile.clone());
        if state.probing.insert(profile.clone()) {
            tokio::spawn(Arc::clone(self).probe(profile.clone()));
        }
    }

    /// Exponential backoff from one minute to one hour, restarting daily.
    async fn probe(self: Arc<Self>, profile: Profile) {
        let mut delay = PROBE_FIRST_DELAY;
        let mut cycle_start = Instant::now();
        loop {
            tokio::time::sleep(delay).await;
            {
                let state = self.lock();
                if !state.benched.contains(&profile) || !state.profiles.contains(&profile) {
                    break;
                }
            }
            if translate::translate(&self.client, "hi", &profile, &mut |_| {})
                .await
                .is_ok()
            {
                self.lock().benched.remove(&profile);
                break;
            }
            if cycle_start.elapsed() >= PROBE_CYCLE {
                delay = PROBE_FIRST_DELAY;
                cycle_start = Instant::now();
            } else {
                delay = (delay * 2).min(PROBE_MAX_DELAY);
            }
        }
        self.lock().probing.remove(&profile);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use super::*;

    /// A one-response-per-connection HTTP server; returns its base URL and hit count.
    async fn serve(status: &'static str, body: &'static str) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0; 4096];
                    while !request_complete(&request) {
                        match socket.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buffer[..n]),
                        }
                    }
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\
                         connection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        (url, hits)
    }

    fn request_complete(request: &[u8]) -> bool {
        let text = String::from_utf8_lossy(request);
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            return false;
        };
        let length = head
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().to_owned())
            })
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        body.len() >= length
    }

    fn profile(url: &str) -> Profile {
        Profile {
            api_base_url: url.into(),
            api_key: "k".into(),
            model: "m".into(),
        }
    }

    #[tokio::test]
    async fn fails_over_and_benches_the_broken_profile() {
        let (broken, broken_hits) = serve("500 Internal Server Error", "boom").await;
        let (healthy, _) = serve(
            "200 OK",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: [DONE]\n\n",
        )
        .await;
        let client = Client::builder().no_proxy().build().unwrap();
        let router = Router::new(
            client,
            &[profile(&broken), Profile::default(), profile(&healthy)],
        );

        let mut seen = Vec::new();
        let translation = router
            .translate("你好", |partial| seen.push(partial.to_owned()))
            .await
            .unwrap();
        assert_eq!(translation.text, "Hello");
        assert!(translation.source_is_chinese);
        assert_eq!(seen, ["", "Hello"], "failover clears partial output first");

        router.translate("你好", |_| {}).await.unwrap();
        assert_eq!(
            broken_hits.load(Ordering::SeqCst),
            1,
            "benched profile is skipped"
        );

        router.set_profiles(&[profile(&broken)]);
        let error = router.translate("你好", |_| {}).await.unwrap_err();
        assert!(error.to_string().contains("500"), "{error:#}");
    }

    #[tokio::test]
    async fn refuses_without_complete_profiles() {
        let router = Router::new(Client::new(), &[Profile::default()]);
        assert!(router.translate("hi", |_| {}).await.is_err());
    }
}
