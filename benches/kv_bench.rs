use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use rustydb::KVStore;

fn benchmark_set(c: &mut Criterion) {
    let store = KVStore::new();

    c.bench_function("set_operation", |b| {
        let mut counter = 0;
        b.iter(|| {
            let key = format!("key_{}", counter);
            let value = format!("value_{}", counter);
            store.set(black_box(key), black_box(value)).unwrap();
            counter += 1;
        });
    });
}

fn benchmark_get(c: &mut Criterion) {
    let store = KVStore::new();

    for i in 0..1000 {
        store
            .set(format!("key_{}", i), format!("value_{}", i))
            .unwrap();
    }

    c.bench_function("get_operation", |b| {
        let mut counter = 0;
        b.iter(|| {
            let key = format!("key_{}", counter % 1000);
            store.get(black_box(&key)).unwrap();
            counter += 1;
        });
    });
}

fn benchmark_mixed_operations(c: &mut Criterion) {
    let store = KVStore::new();

    c.bench_function("mixed_operations", |b| {
        let mut counter = 0;
        b.iter(|| {
            // 70% reads, 30% writes (typical workload)
            if counter % 10 < 7 {
                let key = format!("key_{}", counter % 1000);
                store.get(black_box(&key)).unwrap();
            } else {
                let key = format!("key_{}", counter);
                let value = format!("value_{}", counter);
                store.set(black_box(key), black_box(value)).unwrap();
            }
            counter += 1;
        });
    });
}

fn benchmark_concurrent_reads(c: &mut Criterion) {
    use std::thread;

    let store = KVStore::new();

    for i in 0..1000 {
        store
            .set(format!("key_{}", i), format!("value_{}", i))
            .unwrap();
    }

    c.bench_function("concurrent_reads_4_threads", |b| {
        b.iter(|| {
            let mut handles = vec![];

            for _ in 0..4 {
                let store_clone = store.clone();
                let handle = thread::spawn(move || {
                    for i in 0..250 {
                        let key = format!("key_{}", i % 1000);
                        store_clone.get(&key).unwrap();
                    }
                });
                handles.push(handle);
            }

            for handle in handles {
                handle.join().unwrap();
            }
        });
    });
}

fn benchmark_different_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("value_sizes");

    for size in [10, 100, 1000, 10000].iter() {
        let store = KVStore::new();
        let value = "x".repeat(*size);

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            let mut counter = 0;
            b.iter(|| {
                let key = format!("key_{}", counter);
                store.set(black_box(key), black_box(value.clone())).unwrap();
                counter += 1;
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    benchmark_set,
    benchmark_get,
    benchmark_mixed_operations,
    benchmark_concurrent_reads,
    benchmark_different_sizes
);
criterion_main!(benches);
