use std::hint::black_box;
use std::sync::LazyLock;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use mdtext::{Options, Parser};
use pulldown_cmark::{self as pcm, Options as PcmOptions};
use rand::SeedableRng;
use rand::distr::{Distribution, uniform::Uniform};
use rand::rngs::SmallRng;

const SEED: u64 = 42;
const DOCUMENTS: usize = 100;
const DOC_LEN: usize = 16384;

fn gen_doc(rng: &mut SmallRng) -> String {
    let idx = Uniform::new(0u8, 128u8).unwrap();
    (0..DOC_LEN).map(|_| char::from(idx.sample(rng))).collect()
}

static CORPUS: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut rng = SmallRng::seed_from_u64(SEED);
    (0..DOCUMENTS).map(|_| gen_doc(&mut rng)).collect()
});

fn corpus_bytes() -> usize {
    CORPUS.iter().map(|d| d.len()).sum()
}

fn pcm_gfm_options() -> PcmOptions {
    let mut opts = PcmOptions::empty();
    opts.insert(pcm::Options::ENABLE_TABLES);
    opts.insert(pcm::Options::ENABLE_STRIKETHROUGH);
    opts.insert(pcm::Options::ENABLE_TASKLISTS);
    opts
}

// Pulldown-cmark does not implement full GFM, so compensate
fn mdtext_gfm_options() -> Options {
    Options::TABLES | Options::TASK_LISTS | Options::STRIKETHROUGH
}

fn bench_mdtext(c: &mut Criterion) {
    let bytes = corpus_bytes();
    let mut group = c.benchmark_group("mdtext");
    group.throughput(Throughput::Bytes(bytes as u64));
    group.measurement_time(Duration::from_secs(120));

    group.bench_function("parse", |b| {
        b.iter(|| {
            for doc in CORPUS.iter() {
                let mut parser = Parser::with_options(mdtext_gfm_options());
                let mut iter = parser.feed(doc);
                for event in iter.by_ref() {
                    black_box(event);
                }
                let consumed = iter.consumed();
                std::mem::drop(iter);
                let remaining = if consumed > 0 { &doc[consumed..] } else { doc };
                for event in parser.finish(remaining) {
                    black_box(event);
                }
            }
        });
    });

    group.finish();
}

fn bench_pcm(c: &mut Criterion) {
    let bytes = corpus_bytes();
    let opts = pcm_gfm_options();
    let mut group = c.benchmark_group("pulldown_cmark");
    group.throughput(Throughput::Bytes(bytes as u64));
    group.measurement_time(Duration::from_secs(120));

    group.bench_function("parse", |b| {
        b.iter(|| {
            for doc in CORPUS.iter() {
                let parser = pcm::Parser::new_ext(doc, opts);
                for event in parser {
                    black_box(event);
                }
            }
        });
    });

    group.finish();
}

criterion_group!(benches, bench_mdtext, bench_pcm);
criterion_main!(benches);
