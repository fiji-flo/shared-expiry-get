use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use futures::Future;
use futures::FutureExt;
use futures::TryFutureExt;
use headers::CacheControl;
use headers::HeaderMapExt;
use reqwest;
use serde_json::Value;
use shared_expiry_get::Expiry;
use shared_expiry_get::ExpiryFut;
use shared_expiry_get::ExpiryGetError;
use shared_expiry_get::Provider;
use shared_expiry_get::RemoteStore;

struct HttpProvider {
    url: String,
}

#[derive(Clone)]
struct HttpGet {
    payload: String,
    valid_till: DateTime<Utc>,
}

impl Expiry for HttpGet {
    fn valid(&self) -> bool {
        Utc::now() < self.valid_till
    }
}

impl Provider<HttpGet> for HttpProvider {
    fn update(&self) -> ExpiryFut<HttpGet> {
        reqwest::get(reqwest::Url::parse(&self.url).unwrap())
            .map_ok(move |res| {
                let headers = res.headers();
                let cc: Option<CacheControl> = headers.typed_get();
                let max_age = Duration::from_std(cc.unwrap().max_age().unwrap()).unwrap();
                (res, max_age)
            })
            .and_then(move |(res, max_age)| res.json::<Value>().map_ok(move |j| (j, max_age)))
            .map_ok(move |(j, max_age)| HttpGet {
                payload: serde_json::to_string_pretty(&j).unwrap(),
                valid_till: Utc::now() + max_age,
            })
            .map_err(|e| ExpiryGetError::UpdateFailed(e.to_string()))
            .boxed()
    }
}

fn get() -> impl Future<Output = Result<(), ()>> {
    let remote_store = RemoteStore::new(HttpProvider {
        url: String::from("https://www.mozilla.org/contribute.json"),
    });
    remote_store
        .get()
        .map_ok(|sf| {
            println!("payload: '{}', valid until: {}", sf.payload, sf.valid_till);
        })
        .map_err(|e| {
            println!("something went wrong: {}", e);
            panic!();
        })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tokio::spawn(get()).await?;
    Ok(())
}
