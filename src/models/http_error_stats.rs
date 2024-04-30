use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Eq, Clone, Serialize, Deserialize)]
pub struct HttpErrKey {
    pub name: String,
    pub code: u16,
    pub msg: String,
    pub url: String,
    pub source: String,
    pub host: String,
    pub path: String,
}

impl PartialEq for HttpErrKey {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.url == other.url
            && self.code == other.code
            && self.msg == other.msg
            && self.source == other.source
    }
}

impl Hash for HttpErrKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        format!(
            "{:?}{:?}{:?}{:?}{:?}",
            self.name, self.url, self.code, self.msg, self.source
        )
        .hash(state);
    }
}

pub struct HttpErrorStats {
    pub(crate) errors: Arc<Mutex<HashMap<HttpErrKey, u32>>>,
}

impl HttpErrorStats {
    pub(crate) fn new() -> Self {
        HttpErrorStats {
            errors: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn increment(
        &self,
        name: String,
        url_option: Option<&Url>,
        code: u16,
        msg: String,
        source: String,
    ) {
        let mut url = "-".to_string();
        let mut host = "-".to_string();
        let mut path = "-".to_string();
        if let Some(u) = url_option {
            url = u.to_string();
            if let Some(h) = u.host() {
                host = h.to_string();
            };
            path = u.path().to_string();
        };
        let mut errors = self.errors.lock().await;
        *errors
            .entry(HttpErrKey {
                name,
                code,
                msg,
                url,
                source,
                host,
                path,
            })
            .or_insert(0) += 1;
    }
}
