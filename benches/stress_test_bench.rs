use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use rustydb::KVStore;
use std::thread;
use std::sync::Arc;

fn get_cpu_info() -> (usize, usize) {
    let logical_cpus = num_cpus::get();
    let physical_cpus = num_cpus::get_physical();
    println!("\nCPU Info:");
    println!("   Physical cores: {}", physical_cpus);
    println!("   Logical cores (threads): {}", logical_cpus);
    (physical_cpus, logical_cpus)
}

fn benchmark_concurrent_scaling(c: &mut Criterion) {
    let (physical_cores, logical_cores) = get_cpu_info();
    
    let mut group = c.benchmark_group("stress_concurrent_scaling");
    group.sample_size(50); // Reduce sample size for longer tests
    
    // Test scaling: 1, 2, 4, 8, up to your max threads
    let mut thread_counts = vec![1, 2, 4, 8];
    if physical_cores >= 12 {
        thread_counts.push(12);
    }
    if physical_cores >= 16 {
        thread_counts.push(16);
    }
    if logical_cores >= 24 {
        thread_counts.push(24);
    }
    if logical_cores >= 32 {
        thread_counts.push(32);
    }
    
    for thread_count in thread_counts.iter() {
        let store = Arc::new(KVStore::new());
        
        // Pre-populate with data
        println!("Pre-populating store for {} thread test...", thread_count);
        for i in 0..10000 {
            store.set(format!("key_{}", i), format!("value_{}", i)).unwrap();
        }
        
        group.bench_with_input(
            BenchmarkId::from_parameter(thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let mut handles = vec![];
                    
                    for _ in 0..thread_count {
                        let store_clone = store.clone();
                        let handle = thread::spawn(move || {
                            // Each thread does 1000 reads
                            for i in 0..1000 {
                                let key = format!("key_{}", i % 10000);
                                black_box(store_clone.get(&key).unwrap());
                            }
                        });
                        handles.push(handle);
                    }
                    
                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }
    
    group.finish();
}

fn benchmark_max_read_throughput(c: &mut Criterion) {
    let (_physical_cores, logical_cores) = get_cpu_info();
    let store = Arc::new(KVStore::new());
    
    // Pre-populate
    println!("Pre-populating store for max throughput test...");
    for i in 0..10000 {
        store.set(format!("key_{}", i), format!("value_{}", i)).unwrap();
    }
    
    let bench_name = format!("stress_max_read_throughput_{}_threads", logical_cores);
    
    c.bench_function(&bench_name, |b| {
        b.iter(|| {
            let mut handles = vec![];
            
            // Use ALL your logical cores!
            for _ in 0..logical_cores {
                let store_clone = store.clone();
                let handle = thread::spawn(move || {
                    for i in 0..1000 {
                        let key = format!("key_{}", i % 10000);
                        black_box(store_clone.get(&key).unwrap());
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

fn benchmark_concurrent_writes(c: &mut Criterion) {
    let (_physical_cores, logical_cores) = get_cpu_info();
    
    let mut group = c.benchmark_group("stress_concurrent_writes");
    group.sample_size(50);
    
    // Test write scaling
    let thread_counts = vec![1, 4, 8, 16, logical_cores.min(32)];
    
    for thread_count in thread_counts.iter() {
        let store = Arc::new(KVStore::new());
        
        group.bench_with_input(
            BenchmarkId::from_parameter(thread_count),
            thread_count,
            |b, &thread_count| {
                b.iter(|| {
                    let mut handles = vec![];
                    
                    for thread_id in 0..thread_count {
                        let store_clone = store.clone();
                        let handle = thread::spawn(move || {
                            // Each thread does 100 writes
                            for i in 0..100 {
                                let key = format!("key_{}_{}", thread_id, i);
                                let value = format!("value_{}", i);
                                store_clone.set(key, value).unwrap();
                            }
                        });
                        handles.push(handle);
                    }
                    
                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }
    
    group.finish();
}

fn benchmark_realistic_load(c: &mut Criterion) {
    let (physical_cores, logical_cores) = get_cpu_info();
    let store = Arc::new(KVStore::new());
    
    // Pre-populate
    println!("Pre-populating store for realistic load test...");
    for i in 0..10000 {
        store.set(format!("key_{}", i), format!("value_{}", i)).unwrap();
    }
    
    // Test with physical cores (usually gives best results for mixed workload)
    let bench_name = format!("stress_realistic_load_{}_threads", physical_cores);
    
    c.bench_function(&bench_name, |b| {
        b.iter(|| {
            let mut handles = vec![];
            
            for thread_id in 0..physical_cores {
                let store_clone = store.clone();
                let handle = thread::spawn(move || {
                    for i in 0..1000 {
                        if i % 10 < 7 {
                            // 70% reads
                            let key = format!("key_{}", (thread_id * 1000 + i) % 10000);
                            black_box(store_clone.get(&key).unwrap());
                        } else {
                            // 30% writes
                            let key = format!("key_{}_{}", thread_id, i);
                            let value = format!("value_{}", i);
                            store_clone.set(key, value).unwrap();
                        }
                    }
                });
                handles.push(handle);
            }
            
            for handle in handles {
                handle.join().unwrap();
            }
        });
    });
    
    // Also test with ALL threads (to see SMT impact)
    let bench_name_all = format!("stress_realistic_load_{}_threads_smt", logical_cores);
    
    c.bench_function(&bench_name_all, |b| {
        b.iter(|| {
            let mut handles = vec![];
            
            for thread_id in 0..logical_cores {
                let store_clone = store.clone();
                let handle = thread::spawn(move || {
                    for i in 0..500 {  // Fewer ops per thread since we have more threads
                        if i % 10 < 7 {
                            // 70% reads
                            let key = format!("key_{}", (thread_id * 500 + i) % 10000);
                            black_box(store_clone.get(&key).unwrap());
                        } else {
                            // 30% writes
                            let key = format!("key_{}_{}", thread_id, i);
                            let value = format!("value_{}", i);
                            store_clone.set(key, value).unwrap();
                        }
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

fn benchmark_cache_effects(c: &mut Criterion) {
    let mut group = c.benchmark_group("stress_cache_effects");
    
    // Test different dataset sizes to see L1/L2/L3 cache effects
    for data_size in [100, 1000, 10000, 100000].iter() {
        let store = Arc::new(KVStore::new());
        
        // Pre-populate
        for i in 0..*data_size {
            store.set(format!("key_{}", i), format!("value_{}", i)).unwrap();
        }
        
        group.bench_with_input(
            BenchmarkId::from_parameter(data_size),
            data_size,
            |b, &data_size| {
                b.iter(|| {
                    let mut handles = vec![];
                    
                    for _ in 0..8 {
                        let store_clone = store.clone();
                        let handle = thread::spawn(move || {
                            for i in 0..1000 {
                                let key = format!("key_{}", i % data_size);
                                black_box(store_clone.get(&key).unwrap());
                            }
                        });
                        handles.push(handle);
                    }
                    
                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }
    
    group.finish();
}

criterion_group!(
    benches,
    benchmark_concurrent_scaling,
    benchmark_max_read_throughput,
    benchmark_concurrent_writes,
    benchmark_realistic_load,
    benchmark_cache_effects
);
criterion_main!(benches);