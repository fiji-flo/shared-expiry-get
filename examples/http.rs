extern crate chrono;
extern crate failure;
extern crate futures;
extern crate headers;
extern crate reqwest;
extern crate serde_json;
extern crate shared_expiry_get;
extern crate tokio;

use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use failure::Error;
use futures::Future;
use headers::CacheControl;
use headers::HeaderMapExt;
use reqwest::r#async::Client;
use serde_json::Value;
use shared_expiry_get::Expiry;
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
    fn update(&self) -> Box<Future<Item = HttpGet, Error = Error> + Send> {
        Box::new(
            Client::new()
                .get(&self.url)
                .send()
                .map_err(Into::into)
                .and_then(|mut res| {
                    let headers = res.headers_mut();
                    let cc: Option<CacheControl> = headers.typed_get();
                    let max_age = Duration::from_std(cc.unwrap().max_age().unwrap()).unwrap();
                    res.json::<Value>().map(move |j| HttpGet {
                        payload: serde_json::to_string_pretty(&j).unwrap(),
                        valid_till: Utc::now() + max_age,
                    })
                })
                .map_err(Into::into),
        )
    }
}

fn get() -> impl Future<Item = (), Error = ()> {
    let remote_store = RemoteStore::new(HttpProvider {
        url: String::from("https://www.mozilla.org/contribute.json"),
    });
    remote_store
        .get()
        .map(|sf| {
            println!("payload: '{}', valid until: {}", sf.payload, sf.valid_till);
        })
        .map_err(|e| {
            println!("something went wrong: {}", e);
            panic!();
        })
}

fn main() {
    tokio::run(get());
}
