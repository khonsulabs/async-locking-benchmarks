// This is not a benchmark that is meant to be taken seriously at this time. It
// was written purely to help test an in-development async runtime that this
// database will benefit from.
//
// The problems with the current speed of this database hinge on how
// ACID-compliant you wnat your data writes to be. As of writing this, there are
// no configuration options to change this, but eventually you will have control
// over whether to flush after every write or to flush periodically. Flushing
// periodically will drastically improve speed, but it potentially will lead to
// lost transactions.
//
// When operating `BonsaiDb` in a local or single-server mode, we must recommend
// flushing on each write -- the default configuration. Comparatively speaking,
// this will hurt performance in many benchmarks, including this one below. The
// purpose of this benchmark is to help test the blocking nature of sled within
// an async interface when properly marking each interaction with sled as
// blocking to the async runtime.
//
// Once clustering is available, it will be recommended to have enough
// redundancy in your architecture to allow running the cluster with periodic
// flushing enabled. Because the quorum will self-correct when an individual
// node loses data, as long as you design with enough redundancy in your
// cluster, the risk of data loss goes down drastically.
//
// TODO Some of this explanation eventually should be moved somewhere more useful

use std::{collections::VecDeque, future::Future, sync::Arc};

use async_trait::async_trait;
use criterion::{
    criterion_group, criterion_main, measurement::WallTime, BenchmarkGroup, BenchmarkId, Criterion,
};
use tokio::runtime::Runtime;

fn criterion_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("AsyncLock-Mutex");
    benchmark_lock::<async_lock::Mutex<()>, _, _>(&mut group, "lock", lock);
    benchmark_lock::<async_lock::Mutex<()>, _, _>(&mut group, "try_lock", try_lock);
    group.finish();

    let mut group = c.benchmark_group("AsyncLock-RwLock");
    benchmark_lock::<async_lock::RwLock<()>, _, _>(&mut group, "lock", lock);
    benchmark_lock::<async_lock::RwLock<()>, _, _>(&mut group, "try_lock", try_lock);
    group.finish();

    let mut group = c.benchmark_group("tokio-Mutex");
    benchmark_lock::<tokio::sync::Mutex<()>, _, _>(&mut group, "lock", lock);
    benchmark_lock::<tokio::sync::Mutex<()>, _, _>(&mut group, "try_lock", try_lock);
    group.finish();

    let mut group = c.benchmark_group("tokio-RwLock");
    benchmark_lock::<tokio::sync::RwLock<()>, _, _>(&mut group, "lock", lock);
    benchmark_lock::<tokio::sync::RwLock<()>, _, _>(&mut group, "try_lock", try_lock);
    group.finish();
}

fn benchmark_lock<L: Lockable, Bench: Fn(Arc<L>) -> F, F: Future<Output = ()>>(
    group: &mut BenchmarkGroup<WallTime>,
    name: &str,
    bench: Bench,
) {
    let runtime = tokio::runtime::Runtime::new().unwrap();

    for contention in [0_u32, 1, 2, 3, 5, 10, 25, 50, 100] {
        lock_contention_bench::<L, Bench, F>(&runtime, group, contention, name, &bench);
    }
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);

fn spawn_stoppable_task<
    C: FnOnce(flume::Receiver<()>) -> F,
    F: Future<Output = ()> + Send + 'static,
>(
    runtime: &tokio::runtime::Runtime,
    task: C,
) -> flume::Sender<()> {
    let (sender, receiver) = flume::bounded(1);

    runtime.spawn(task(receiver));

    sender
}

fn lock_contention_bench<L: Lockable, Bench: Fn(Arc<L>) -> F, F: Future<Output = ()>>(
    runtime: &Runtime,
    group: &mut BenchmarkGroup<WallTime>,
    contention: u32,
    name: &str,
    bench: &Bench,
) {
    let contention_percent = if contention > 0 { 100 / contention } else { 0 };
    group.bench_with_input(
        BenchmarkId::from_parameter(format!("{}-{:02}", name, contention_percent)),
        &contention,
        |b, _| {
            let mut mutexes = VecDeque::new();
            for _ in 0_u32..contention.max(1) {
                mutexes.push_back(Arc::new(L::new()));
            }
            let stop = spawn_stoppable_task(runtime, |stop| {
                let mut mutexes = mutexes.clone();
                async move {
                    if contention > 0 {
                        while stop.try_recv().is_err() {
                            let mutex = mutexes.pop_front().unwrap();
                            mutex.lock().await;
                            mutexes.push_back(mutex);
                        }
                    }
                }
            });
            b.to_async(runtime).iter(|| {
                let lock = mutexes[0].clone();
                bench(lock)
            });
            let _ = stop.send(());
        },
    );
}

#[derive(Debug)]
pub enum Backend {
    Tokio,
    AsyncLock,
}

#[async_trait]
trait Lockable: Send + Sync + 'static {
    const BACKEND: Backend;

    fn new() -> Self;
    async fn lock(&self);
    fn try_lock(&self) -> bool;
}

#[async_trait]
impl Lockable for async_lock::Mutex<()> {
    const BACKEND: Backend = Backend::AsyncLock;

    fn new() -> Self {
        Self::new(())
    }

    async fn lock(&self) {
        self.lock().await;
    }

    fn try_lock(&self) -> bool {
        self.try_lock().is_some()
    }
}

#[async_trait]
impl Lockable for async_lock::RwLock<()> {
    const BACKEND: Backend = Backend::AsyncLock;

    fn new() -> Self {
        Self::new(())
    }

    async fn lock(&self) {
        self.write().await;
    }

    fn try_lock(&self) -> bool {
        self.try_write().is_some()
    }
}

#[async_trait]
impl Lockable for tokio::sync::Mutex<()> {
    const BACKEND: Backend = Backend::Tokio;

    fn new() -> Self {
        Self::new(())
    }

    async fn lock(&self) {
        self.lock().await;
    }

    fn try_lock(&self) -> bool {
        self.try_lock().is_ok()
    }
}

#[async_trait]
impl Lockable for tokio::sync::RwLock<()> {
    const BACKEND: Backend = Backend::Tokio;

    fn new() -> Self {
        Self::new(())
    }

    async fn lock(&self) {
        self.write().await;
    }

    fn try_lock(&self) -> bool {
        self.try_write().is_ok()
    }
}

async fn lock<L: Lockable>(lock: Arc<L>) {
    lock.lock().await;
}

async fn try_lock<L: Lockable>(lock: Arc<L>) {
    if !lock.try_lock() {
        lock.lock().await;
    }
}
