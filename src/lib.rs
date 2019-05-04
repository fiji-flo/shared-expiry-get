#[macro_use]
extern crate failure;
extern crate futures;
#[macro_use]
extern crate failure_derive;

use failure::Error;
use futures::future;
use futures::future::Shared;
use futures::Future;
use futures::IntoFuture;
use std::clone::Clone;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;

#[derive(Debug, Fail)]
pub enum CondvarStoreError {
    #[fail(display = "Timeout while waiting for GET")]
    GetTimeout,
    #[fail(display = "Poisoned lock: {}", _0)]
    PoisonedLock(String),
    #[fail(display = "not initialized")]
    NotInitialized,
    #[fail(display = "unknown error")]
    Unknown,
}

pub trait Expiry {
    fn valid(&self) -> bool;
}

#[derive(Clone)]
pub struct Fu<T: Expiry + Clone + 'static> {
    pub f: Shared<Box<Future<Item = T, Error = Error> + Send>>,
}

pub trait Provider<T: Expiry + Clone + 'static> {
    fn update(&self) -> Box<Future<Item = T, Error = Error> + Send>;
}

pub struct RemoteStore<T: Expiry + Clone + Sync + Send + 'static, P: Provider<T> + 'static> {
    pub provider: Arc<P>,
    pub remote: Arc<RwLock<Fu<T>>>,
    pub inflight: Arc<Mutex<bool>>,
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

impl<T: Expiry + Clone + Sync + Send + 'static, P: Provider<T> + 'static> RemoteStore<T, P> {
    #[allow(clippy::mutex_atomic)]
    pub fn new(p: P) -> Self {
        let remote = Arc::new(RwLock::new(Fu {
            f: Future::shared(p.update()),
        }));
        RemoteStore {
            provider: Arc::new(p),
            remote,
            inflight: Arc::new(Mutex::new(false)),
        }
    }

    pub fn update(self) -> Shared<Box<Future<Item = T, Error = Error> + Send>> {
        if let Ok(mut lock) = self.inflight.lock() {
            if !*lock {
                *lock = true;
                if let Ok(mut r) = self.remote.write() {
                    let unlock = Arc::clone(&self.inflight);
                    let f = self.provider.update().then(move |f| {
                        if let Ok(mut unlock) = unlock.lock() {
                            *unlock = false;
                        }
                        f
                    });
                    *r = Fu {
                        f: Future::shared(Box::new(f)),
                    };
                }
            }
            if let Ok(r) = self.remote.read() {
                return r.f.clone();
            }
        }
        Future::shared(Box::new(
            future::err(format_err!("no update")).into_future(),
        ))
    }

    fn get_or_update(self, t: T) -> Box<Future<Item = T, Error = Error>> {
        if t.valid() {
            Box::new(future::ok::<T, Error>(t))
        } else {
            Box::new(
                self.update()
                    .map_err(move |_| format_err!("no inner"))
                    .map(|i| (*i).clone()),
            )
        }
    }

    pub fn get(&self) -> Shared<Box<Future<Item = T, Error = Error>>> {
        let s = (*self).clone();
        match self.remote.read() {
            Ok(ref f) => Future::shared(Box::new(
                f.f.clone()
                    .map_err(move |_| format_err!("no inner"))
                    .and_then(move |item| s.get_or_update((*item).clone())),
            )),
            Err(e) => Future::shared(Box::new(
                future::err(CondvarStoreError::PoisonedLock(e.to_string()).into()).into_future(),
            )),
        }
    }
}

#[cfg(test)]
mod test {
    extern crate chrono;
    extern crate futures_timer;
    use super::*;
    use chrono::DateTime;
    use chrono::Utc;
    use futures::future::SharedError;
    use futures::future::SharedItem;
    use futures_timer::Delay;
    use std::sync::atomic::AtomicI64;
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::Duration;

    struct P1 {
        counter: Arc<AtomicI64>,
    }
    struct P2 {}

    #[derive(Clone)]
    struct E1 {
        expire: DateTime<Utc>,
        payload: usize,
    }

    impl Expiry for E1 {
        fn valid(&self) -> bool {
            self.expire > Utc::now()
        }
    }

    impl Provider<E1> for P1 {
        fn update(&self) -> Box<Future<Item = E1, Error = Error> + Send> {
            println!("started update");
            let c = Arc::clone(&self.counter);
            Box::new(
                Delay::new(Duration::from_millis(10))
                    .map(move |_| {
                        println!("done update");
                        c.fetch_add(1, Ordering::SeqCst);
                        E1 {
                            expire: Utc::now() + chrono::Duration::milliseconds(200),
                            payload: 0,
                        }
                    })
                    .map_err(Into::into)
                    .into_future(),
            )
        }
    }

    impl Provider<E1> for P2 {
        fn update(&self) -> Box<Future<Item = E1, Error = Error> + Send> {
            println!("started update");
            Box::new(future::err(format_err!("boom")).into_future())
        }
    }

    fn check_ok_and_expiry<T: Expiry + Clone + 'static>(
        t: Result<SharedItem<T>, SharedError<Error>>,
    ) {
        assert!(t.is_ok());
        let t = t.unwrap();
        assert!(t.valid());
    }

    fn check_counter(counter: &Arc<AtomicI64>, should: i64) {
        assert_eq!(counter.load(Ordering::SeqCst), should);
    }

    #[test]
    fn works() {
        let provider = P1 {
            counter: Arc::new(AtomicI64::default()),
        };
        let counter = Arc::clone(&provider.counter);
        let rs = RemoteStore::new(provider);
        let c = rs.get().wait();
        check_ok_and_expiry(c);

        let mut threads = vec![];
        for _ in 0..10 {
            let rs_c = rs.clone();
            let child = thread::spawn(move || {
                thread::sleep(Duration::from_millis(50));
                let c = rs_c.get().wait();
                check_ok_and_expiry(c);
            });
            threads.push(child);
        }

        let c = rs.get().wait();
        check_ok_and_expiry(c);

        assert!(threads.into_iter().map(|c| c.join()).all(|j| j.is_ok()));
        check_counter(&counter, 1);

        thread::sleep(Duration::from_millis(300));
        check_counter(&counter, 1);

        let rs_c = rs.clone();
        let child = thread::spawn(move || {
            let c = rs_c.get().wait();
            check_ok_and_expiry(c);
        });
        let c = rs.get().wait();
        check_ok_and_expiry(c);
        assert!(child.join().is_ok());
        check_counter(&counter, 2);
    }

    #[test]
    fn many_threads() {
        let rs = RemoteStore::new(P1 {
            counter: Arc::new(AtomicI64::default()),
        });
        let c = rs.get().wait();
        check_ok_and_expiry(c);

        let mut threads = vec![];
        for i in 0..30 {
            let rs_c = rs.clone();
            let child = thread::spawn(move || {
                thread::sleep(Duration::from_millis(i * 10));
                let c = rs_c.get().wait();
                check_ok_and_expiry(c);
            });
            threads.push(child);
        }

        assert!(threads.into_iter().map(|c| c.join()).all(|j| j.is_ok()));
    }

    #[test]
    fn many_threads_fail() {
        let rs = RemoteStore::new(P2 {});
        let _ = rs.get().wait();

        let mut threads = vec![];
        for i in 0..30 {
            let rs_c = rs.clone();
            let child = thread::spawn(move || {
                thread::sleep(Duration::from_millis(i * 10));
                let c = rs_c.get().wait();
                assert!(c.is_err());
            });
            threads.push(child);
        }

        assert!(threads.into_iter().map(|c| c.join()).all(|j| j.is_ok()));
    }
}
