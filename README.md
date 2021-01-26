# Simple concurrent async get with expiration for Rust.
[![Latest Version](https://img.shields.io/crates/v/shared-expiry-get.svg)](https://crates.io/crates/shared-expiry-get)
[![Docs](https://docs.rs/shared-expiry-get/badge.svg)](https://docs.rs/shared-expiry-get)
---

`shared-expiry-get` is a wrapper for accessing and caching a remote data source with some
expiration criteria.

# Features

- retrieve only once even if multiple threads are trying to access the remote data at the same time
- async support
- update data on expiration

`shared-expiry-get` does not:

- retry on error

# Example Use Cases

- cached access of an http source respecting cache control
- cached access of a file which may get updated

# A basic Example

```rust
#[derive(Clone)]
struct MyProvider {}
#[derive(Clone)]
struct Payload {}

impl Expiry for Payload {
    fn valid(&self) -> bool {
        true
    }
}

impl Provider<Payload> for MyProvider {
    fn update(&self) -> ExpiryFut<Payload> {
        future::ok::<Payload, ExpiryGetError>(Payload {}).into_future().boxed()
    }
}

async fn main() {
    let rs = RemoteStore::new(MyProvider {});
    let payload = rs.get().await;
    assert!(payload.is_ok());
}
```
