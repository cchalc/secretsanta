use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use santa::accommodate::minimal_policy;
use santa::domain::synthetic_roster;
use santa::rules::Policy;
use santa::solver::{Budget, count_solutions, solve};

fn bench_solve(c: &mut Criterion) {
    let mut g = c.benchmark_group("solve");
    // (label, participants, household size pattern)
    let cases: &[(&str, usize, &[usize])] = &[
        ("holtby-shape", 6, &[2]),
        ("family-10", 10, &[2, 1, 3]),
        ("party-50", 50, &[1, 2, 3, 4]),
        ("office-200", 200, &[1, 2, 3, 4]),
        ("company-1000", 1000, &[1, 2, 3, 4]),
    ];
    for &(label, n, sizes) in cases {
        let roster = synthetic_roster(n, sizes);
        let mut rng = ChaCha20Rng::seed_from_u64(1);
        g.bench_with_input(BenchmarkId::new("sample", label), &roster, |b, r| {
            b.iter(|| solve(black_box(r), &Policy::strict(), &mut rng, Budget::default()).unwrap())
        });
    }
    // Exhaustive search path (sampling disabled) on small rosters.
    for &(label, n, sizes) in &cases[..3] {
        let roster = synthetic_roster(n, sizes);
        let mut rng = ChaCha20Rng::seed_from_u64(1);
        let budget = Budget {
            sample_attempts: 0,
            ..Budget::default()
        };
        g.bench_with_input(BenchmarkId::new("search", label), &roster, |b, r| {
            b.iter(|| solve(black_box(r), &Policy::strict(), &mut rng, budget).unwrap())
        });
    }
    g.finish();

    let mut g = c.benchmark_group("prove");
    let chalcraft = synthetic_roster(5, &[2, 1, 2]);
    g.bench_function("infeasible-chalcraft-shape", |b| {
        let mut rng = ChaCha20Rng::seed_from_u64(1);
        b.iter(|| {
            solve(
                black_box(&chalcraft),
                &Policy::strict(),
                &mut rng,
                Budget::default(),
            )
            .unwrap_err()
        })
    });
    g.bench_function("accommodate-chalcraft-shape", |b| {
        b.iter(|| minimal_policy(black_box(&chalcraft), &Policy::strict(), u64::MAX).unwrap())
    });
    let ten = synthetic_roster(10, &[2, 1, 3]);
    g.bench_function("count-family-10", |b| {
        b.iter(|| count_solutions(black_box(&ten), &Policy::strict(), u64::MAX, u64::MAX))
    });
    g.finish();
}

criterion_group!(benches, bench_solve);
criterion_main!(benches);
