//! # shared-expiry-get
//! `shared-expiry-get` is a wrapper for accessing and caching a remote data source with some
//! expiration criteria.
//!
//! # A basic Example
//!
//! ```
//! #[allow(clippy::needless_doctest_main)]
//! use futures::executor::block_on;
//! use futures::future;
//! use futures::future::FutureExt;
//! use futures::future::IntoFuture;
//! use futures::future::TryFutureExt;
//! use futures::Future;
//! use std::pin::Pin;
//!
//! use shared_expiry_get::Expiry;
//! use shared_expiry_get::ExpiryGetError;
//! use shared_expiry_get::Provider;
//! use shared_expiry_get::RemoteStore;
//!
//! #[derive(Clone)]
//! struct MyProvider {}
//! #[derive(Clone)]
//! struct Payload {}
//!
//! impl Expiry for Payload {
//!     fn valid(&self) -> bool {
//!         true
//!     }
//! }
//!
//! impl Provider<Payload> for MyProvider {
//!     fn update(&self) -> Pin<Box<dyn Future<Output = Result<Payload, ExpiryGetError>> + Send>> {
//!         future::ok::<Payload, ExpiryGetError>(Payload {}).into_future().boxed()
//!     }
//! }
//!
//!
//! let rs = RemoteStore::new(MyProvider {});
//! let payload = block_on(rs.get());
//! assert!(payload.is_ok());
//!
//! ```
extern crate failure;
extern crate futures;
#[macro_use]
extern crate failure_derive;
#[macro_use]
extern crate log;

use futures::future;
use futures::future::FutureExt;
use futures::future::Shared;
use futures::future::TryFutureExt;
use std::clone::Clone;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;

/// Various internal errors.
#[derive(Debug, Fail, Clone)]
pub enum ExpiryGetError {
    /// Internal `Mutex` or `RwLock` is poisoned.
    #[fail(display = "Poisoned lock: {}", _0)]
    PoisonedLock(String),
    /// First `get` happened before initialization. This must not happen.
    #[fail(display = "not initialized")]
    NotInitialized,
    /// Something went wrong during update. Maybe a timeout or invalid data.
    #[fail(display = "Inner update future failed: {}", _0)]
    InnerFutureFailed(String),
    #[fail(display = "Update failed: {}", _0)]
    UpdateFailed(String),
}

pub type ExpiryFut<T> = Pin<Box<dyn Future<Output = Result<T, ExpiryGetError>> + Send>>;

macro_rules! poison_err_future {
    ($e:ident) => {{
        error!("poisoned lock: {}", $e);
        future::err(ExpiryGetError::PoisonedLock($e.to_string()).into())
            .boxed()
            .shared()
    }};
}

/// Used to determine whether the remote data is still valid.
pub trait Expiry {
    /// Return whether the remote data is still valid.
    ///
    /// Example:
    /// ```
    /// # use chrono::{DateTime, Utc};
    /// # use shared_expiry_get::Expiry;
    ///
    /// #[derive(Clone)]
    /// struct E {
    ///     expire: DateTime<Utc>,
    ///     payload: String,
    /// }
    /// impl Expiry for E {
    ///     fn valid(&self) -> bool {
    ///         self.expire > Utc::now()
    ///     }
    /// }
    /// ```
    fn valid(&self) -> bool;
}

#[derive(Clone)]
struct Fu<T: Expiry + Clone + Send + 'static> {
    pub f: Shared<ExpiryFut<T>>,
}

/// Provides updates to _get_ remote data.
pub trait Provider<T: Expiry + Clone + 'static> {
    /// Provide a updated version of the remote data. E.g via an async http get.
    ///
    /// Example:
    /// ```
    /// # use failure::Error;
    /// # use futures::future;
    /// # use futures::future::FutureExt;
    /// # use futures::future::IntoFuture;
    /// # use futures::future::TryFutureExt;
    /// # use futures::Future;
    /// # use std::pin::Pin;
    ///
    /// # use shared_expiry_get::Expiry;
    /// # use shared_expiry_get::ExpiryGetError;
    /// # use shared_expiry_get::Provider;
    /// # use shared_expiry_get::RemoteStore;
    ///
    /// # #[derive(Clone)]
    /// # struct Payload {}
    /// # impl Expiry for Payload {
    /// #     fn valid(&self) -> bool {
    /// #         true
    /// #     }
    /// # }
    /// #[derive(Clone)]
    /// struct MyProvider {}
    ///
    /// impl Provider<Payload> for MyProvider {
    ///     fn update(&self) -> Pin<Box<dyn Future<Output = Result<Payload, ExpiryGetError>> + Send>> {
    ///         future::ok::<Payload, ExpiryGetError>(Payload {}).into_future().boxed()
    ///     }
    /// }
    /// ```
    fn update(&self) -> ExpiryFut<T>;
}

/// A remote store wrapping an updated provider ([`impl Provider`](trait.Provider.html)) and
/// remote data type ([`impl Expiry`](trait.Expiry.html)).
pub struct RemoteStore<T: Expiry + Clone + Sync + Send + 'static, P: Provider<T> + 'static> {
    provider: Arc<P>,
    remote: Arc<RwLock<Fu<T>>>,
    inflight: Arc<Mutex<bool>>,
}

impl<T: Expiry + Clone + Sync + Send + 'static, P: Provider<T> + 'static> Clone
    for RemoteStore<T, P>
{
    fn clone(&self) -> Self {
        RemoteStore {
            provider: Arc::clone(&self.provider),
            remote: Arc::clone(&self.remote),
            inflight: Arc::clone(&self.inflight),
        }
    }
}

impl<T: Expiry + Clone + Sync + Send + 'static, P: Provider<T> + Sync + Send + 'static>
    RemoteStore<T, P>
{
    /// Wrap a [Provider](trait.Provider.html).
    #[allow(clippy::mutex_atomic)]
    pub fn new(p: P) -> Self {
        info!("initializing from remote");
        let remote = Arc::new(RwLock::new(Fu {
            f: FutureExt::shared(p.update()),
        }));
        RemoteStore {
            provider: Arc::new(p),
            remote,
            inflight: Arc::new(Mutex::new(false)),
        }
    }

    fn update(self) -> Shared<ExpiryFut<T>> {
        debug!("lock for updating from remote");
        match self.inflight.lock() {
            Ok(mut lock) => {
                if !*lock {
                    *lock = true;
                    info!("updating from remote");
                    match self.remote.write() {
                        Ok(mut r) => {
                            let unlock = Arc::clone(&self.inflight);
                            let f = self.provider.update().map(move |f| {
                                if let Ok(mut unlock) = unlock.lock() {
                                    *unlock = false;
                                }
                                f
                            });
                            *r = Fu {
                                f: FutureExt::shared(f.boxed()),
                            };
                        }
                        Err(e) => return poison_err_future!(e),
                    }
                } else {
                    debug!("update ongoing")
                }
                match self.remote.read() {
                    Ok(r) => r.f.clone(),
                    Err(e) => return poison_err_future!(e),
                }
            }
            Err(e) => poison_err_future!(e),
        }
    }

    fn get_or_update(self, t: T) -> ExpiryFut<T> {
        debug!("get or update");
        if t.valid() {
            debug!("reusing cached data");
            future::ok::<T, ExpiryGetError>(t).boxed()
        } else {
            self.update()
                .map_err(|e| ExpiryGetError::InnerFutureFailed(e.to_string()))
                .boxed()
        }
    }

    /// Retrieve the remote data implementing [Expiry](trait.Expiry.html). This will either return
    /// cached data or retrieve new data via the [Provider](trait.Provider.html).
    pub fn get(&self) -> ExpiryFut<T> {
        let s = (*self).clone();
        match self.remote.read() {
            Ok(ref f) => {
                debug!("reading remote");
                f.f.clone()
                    .and_then(move |item| s.get_or_update(item))
                    .boxed()
            }
            Err(e) => future::err::<T, ExpiryGetError>(ExpiryGetError::PoisonedLock(e.to_string()))
                .boxed(),
        }
    }
}

#[cfg(test)]
mod test_timed {
    extern crate chrono;
    extern crate futures_timer;
    use super::*;
    use chrono::DateTime;
    use chrono::Utc;
    use futures::executor::block_on;
    use futures_timer::Delay;
    use std::sync::atomic::AtomicI64;
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::Duration;

    struct P1 {
        counter: Arc<AtomicI64>,
    }

    #[derive(Clone)]
    struct E1 {
        expire: DateTime<Utc>,
        payload: String,
    }

    impl Expiry for E1 {
        fn valid(&self) -> bool {
            self.expire > Utc::now()
        }
    }

    fn check_ok_and_expiry<T: Expiry + Clone + 'static>(t: Result<T, ExpiryGetError>) {
        assert!(t.is_ok());
        let t = t.unwrap();
        assert!(t.valid());
    }

    impl Provider<E1> for P1 {
        fn update(&self) -> ExpiryFut<E1> {
            let c = Arc::clone(&self.counter);
            Delay::new(Duration::from_millis(10))
                .map(move |_| {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok::<E1, ExpiryGetError>(E1 {
                        expire: Utc::now() + chrono::Duration::milliseconds(200),
                        payload: String::from("foobar"),
                    })
                })
                .map_err(Into::into)
                .into_future()
                .boxed()
        }
    }

    fn check_counter(counter: &Arc<AtomicI64>, should: i64) {
        assert_eq!(counter.load(Ordering::SeqCst), should);
    }

    #[test]
    fn threaded_with_ttl() {
        let provider = P1 {
            counter: Arc::new(AtomicI64::default()),
        };
        let counter = Arc::clone(&provider.counter);
        let rs = RemoteStore::new(provider);
        let c = block_on(rs.get());
        check_ok_and_expiry(c);

        let mut threads = vec![];
        for _ in 0..10 {
            let rs_c = rs.clone();
            let child = thread::spawn(move || {
                async move {
                    thread::sleep(Duration::from_millis(50));
                    let c = rs_c.get().await;
                    check_ok_and_expiry(c);
                }
            });
            threads.push(child);
        }

        let c = block_on(rs.get());
        check_ok_and_expiry(c);

        assert!(threads.into_iter().map(|c| c.join()).all(|j| j.is_ok()));
        check_counter(&counter, 1);

        thread::sleep(Duration::from_millis(300));
        check_counter(&counter, 1);

        let rs_c = rs.clone();
        let child = thread::spawn(move || {
            let c = block_on(rs_c.get());
            check_ok_and_expiry(c);
        });
        let c = block_on(rs.get());
        check_ok_and_expiry(c);
        assert!(child.join().is_ok());
        check_counter(&counter, 2);
    }

    #[test]
    fn many_threads() {
        let rs = RemoteStore::new(P1 {
            counter: Arc::new(AtomicI64::default()),
        });
        let c = block_on(rs.get());
        check_ok_and_expiry(c);

        let mut threads = vec![];
        for i in 0..30 {
            let rs_c = rs.clone();
            let child = thread::spawn(move || {
                thread::sleep(Duration::from_millis(i * 10));
                let c = block_on(rs_c.get());
                check_ok_and_expiry(c);
            });
            threads.push(child);
        }

        assert!(threads.into_iter().map(|c| c.join()).all(|j| j.is_ok()));
    }
}
#[cfg(test)]
mod test_counted {
    use super::*;
    use futures::executor::block_on;
    use futures_timer::Delay;
    use std::sync::atomic::AtomicI64;
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::Duration;

    struct P2 {
        counter: Arc<AtomicI64>,
        valid_for: i64,
    }

    #[derive(Clone)]
    struct E2 {
        counter: Arc<AtomicI64>,
        payload: usize,
        valid_for: i64,
    }

    impl Expiry for E2 {
        fn valid(&self) -> bool {
            let c = self.counter.load(Ordering::SeqCst);
            self.valid_for > c
        }
    }

    impl Provider<E2> for P2 {
        fn update(&self) -> ExpiryFut<E2> {
            let c = Arc::clone(&self.counter);
            let valid_for = self.valid_for;
            Delay::new(Duration::from_millis(10))
                .map(move |_| {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok::<E2, ExpiryGetError>(E2 {
                        counter: Arc::new(AtomicI64::new(0)),
                        payload: 0,
                        valid_for,
                    })
                })
                .map_err(Into::into)
                .into_future()
                .boxed()
        }
    }

    fn check_and_increment(t: Result<E2, ExpiryGetError>) {
        assert!(t.is_ok());
        let t = t.unwrap();
        (*t.counter).fetch_add(1, Ordering::SeqCst);
    }

    fn check_counter(counter: &Arc<AtomicI64>, should: i64) {
        assert_eq!(counter.load(Ordering::SeqCst), should);
    }

    #[test]
    fn threaded_with_counter() {
        let provider = P2 {
            counter: Arc::new(AtomicI64::default()),
            valid_for: 10,
        };
        let counter = Arc::clone(&provider.counter);
        let rs = RemoteStore::new(provider);
        let c = block_on(rs.get());
        check_and_increment(c);
        check_counter(&counter, 1);

        let mut threads = vec![];
        for _ in 0..8 {
            let rs_c = rs.clone();
            let child = thread::spawn(move || {
                let c = block_on(rs_c.get());
                check_and_increment(c);
            });
            threads.push(child);
        }

        let c = block_on(rs.get());
        check_and_increment(c);

        assert!(threads.into_iter().map(|c| c.join()).all(|j| j.is_ok()));
        check_counter(&counter, 1);

        let rs_c = rs.clone();
        let child = thread::spawn(move || {
            let c = block_on(rs_c.get());
            check_and_increment(c);
        });
        let c = block_on(rs.get());
        check_and_increment(c);
        assert!(child.join().is_ok());
        check_counter(&counter, 2);
    }
}

#[cfg(test)]
mod test_failing {
    use super::*;
    use futures::executor::block_on;
    use std::thread;
    use std::time::Duration;

    #[derive(Clone)]
    struct P3 {}
    #[derive(Clone)]
    struct E3 {}

    impl Expiry for E3 {
        fn valid(&self) -> bool {
            true
        }
    }

    impl Provider<E3> for P3 {
        fn update(&self) -> ExpiryFut<E3> {
            future::err(ExpiryGetError::NotInitialized.into())
                .into_future()
                .boxed()
        }
    }

    #[test]
    fn many_threads_fail() {
        let rs = RemoteStore::new(P3 {});
        let _ = block_on(rs.get());

        let mut threads = vec![];
        for i in 0..30 {
            let rs_c = rs.clone();
            let child = thread::spawn(move || {
                thread::sleep(Duration::from_millis(i * 10));
                let c = block_on(rs_c.get());
                assert!(c.is_err());
            });
            threads.push(child);
        }

        assert!(threads.into_iter().map(|c| c.join()).all(|j| j.is_ok()));
    }
}
