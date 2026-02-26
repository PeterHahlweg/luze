use criterion::{criterion_group, criterion_main, Criterion};
use luze::{Note, NoteBox, DRAW_CAPACITY};

const NOTES: usize = 90_000;
const CONTENT: &str = "Luhmann had two primary slip boxes: one for thematic notes \
    (his core NoteBox with around 90,000 atomic notes on ideas, concepts, and \
    thoughts, numbered alphanumerically like 57/12a) and a separate bibliographic \
    Kasten for references, books, and sources with IDs like B 123.";

/// Builds a NoteBox with 90k notes spread across draws of DRAW_CAPACITY each.
fn build_zk() -> NoteBox {
    let mut zk = NoteBox::default();
    for i in 0..NOTES {
        let section = i / DRAW_CAPACITY;
        let local   = i % DRAW_CAPACITY;
        let id = if section == 0 {
            format!("{local}")
        } else {
            format!("s{section}/{local}")
        };
        zk.add(Note::new(id.as_str(), "1", CONTENT)).unwrap();
    }
    zk
}

fn bench_serialize(c: &mut Criterion) {
    let zk = build_zk();
    // Collect all notes into a flat Vec<Note> and measure sonic_rs serialisation.
    let notes: Vec<Note> = zk.notes().into_iter().cloned().collect();
    c.bench_function("serialize 90k notes", |b| {
        b.iter(|| sonic_rs::to_string(&notes).unwrap())
    });
}

fn bench_deserialize(c: &mut Criterion) {
    let zk = build_zk();
    let notes: Vec<Note> = zk.notes().into_iter().cloned().collect();
    let json = sonic_rs::to_string(&notes).unwrap();
    c.bench_function("deserialize 90k notes", |b| {
        b.iter(|| sonic_rs::from_str::<Vec<Note>>(&json).unwrap())
    });
}

fn bench_insert(c: &mut Criterion) {
    c.bench_function("insert 90k notes", |b| {
        b.iter(build_zk)
    });
}

criterion_group!(benches, bench_serialize, bench_deserialize, bench_insert);
criterion_main!(benches);
