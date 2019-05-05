# Simple concurrent async get with expiration for Rust.
[![Build Status](https://travis-ci.org/fiji-flo/shared-expiry-get.svg?branch=master)](https://travis-ci.org/fiji-flo/shared-expiry-get)
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

- do lazy initialization (first get happens at creation)
- retry on error

# Example Use Cases

- cached access of an http source respecting cache control
- cached access of a file which may get updated

# A basic Example

```rust
use failure::Error;
use futures::future;
use futures::future::IntoFuture;
use futures::Future;

use shared_expiry_get::Expiry;
use shared_expiry_get::Provider;
use shared_expiry_get::RemoteStore;

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
    fn update(&self) -> Box<Future<Item = Payload, Error = Error> + Send> {
        Box::new(future::ok(Payload {}).into_future())
    }
}

fn main() {
    let rs = RemoteStore::new(MyProvider {});
    let payload = rs.get().wait();
    assert!(payload.is_ok());
}
```