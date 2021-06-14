#![feature(
    arbitrary_self_types, // Unfortunately this is required for methods on Gc refs
)]
use std::cell::{Cell, RefCell};

use slog::{Logger, Drain, o};

use zerogc_simple::{Gc, SimpleCollector, SimpleCollectorContext, CollectorId as SimpleCollectorId};
use zerogc_derive::{Trace, NullTrace, unsafe_gc_impl};
use zerogc::prelude::*;
use std::io::Write;

const IM: i32 = 139968;
const IA: i32 = 3877;
const IC: i32 = 29573;

const LINE_LENGTH: usize = 60;
const BUFFER_SIZE: usize = (LINE_LENGTH + 1)*1024; // add 1 for '\n'

/// Weighted selection from alphabet
const ALU: &'static str = concat!(
    "GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGG",
    "GAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGA",
    "CCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAAT",
    "ACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCA",
    "GCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGG",
    "AGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCC",
    "AGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA"
);

#[derive(NullTrace)]
struct State {
    last: Cell<i32>
}
impl State {
    fn new() -> State {
        State {
            last: Cell::new(42) // we want determinism
        }
    }
    /// Psuedo random number generator
    fn random(&self, max: f32) -> f32 {
        const ONE_OVEER_IM: f32 = 1.0f32 / IM as f32;
        self.last.set((self.last.get() * IA + IC) % IM);
        return max * self.last.get() as f32 * ONE_OVEER_IM;
    }
}

#[derive(Trace)]
#[zerogc(collector_id(SimpleCollectorId))]
struct FloatProbFreq<'gc> {
    chars: Gc<'gc, Vec<Cell<u8>>>,
    probs: Gc<'gc, Vec<Cell<f32>>>
}
impl<'gc> FloatProbFreq<'gc> {
    pub fn alloc(gc: &'gc SimpleCollectorContext, chars: Gc<'gc, Vec<Cell<u8>>>, probs: &[f64]) -> Gc<'gc, FloatProbFreq<'gc>> {
        let probs = gc.alloc(probs.iter().map(|&f| f as f32).map(Cell::new).collect::<Vec<_>>());
        gc.alloc(FloatProbFreq { chars, probs })
    }
    pub fn make_cumulative(&self) {
        let mut cp = 0.0f64;
        for prob in self.probs.value() {
            cp += prob.get() as f64;
            prob.set(cp as f32);
        }
    }
    pub fn select_random_into_buffer(
        &self, state: &State, buffer: &mut [u8],
        mut buffer_index: usize, nRandom: usize
    ) -> usize {
        let chars = self.chars.value();
        let probs = self.probs.value();
        'outer: for rIndex in 0..nRandom {
            let r = state.random(1.0f32);
            for (i, prob) in probs.iter().enumerate() {
                if r < prob.get() {
                    buffer[buffer_index] = chars[i].get();
                    buffer_index += 1;
                    continue 'outer;
                }
            }
            buffer[buffer_index] = chars[probs.len() - 1].get();
        }
        return buffer_index;
    }
}

#[derive(Trace)]
#[zerogc(collector_id(SimpleCollectorId), copy)]
#[derive(Copy, Clone)]
struct MakeFastaTask<'gc> {
    buffer: Option<Gc<'gc, RefCell<Vec<u8>>>>, // TODO: Replace with Gc slice
    buffer_index: usize,
    n_chars: usize,
    state: Gc<'gc, State>,
    id: Gc<'gc, String>,
    desc: Gc<'gc, String>,
}
impl<'gc> MakeFastaTask<'gc> {
    pub fn new(
        gc: &'gc SimpleCollectorContext,
        state: Gc<'gc, State>,
        id: &str,
        desc: &str,
        n_chars: usize,
    ) -> Self {
        MakeFastaTask {
            buffer: None,
            buffer_index: 0,
            n_chars, state,
            id: gc.alloc(String::from(id)),
            desc: gc.alloc(String::from(desc)),
        }
    }
}

fn make_fasta<'gc1, F: for<'gc> FnOnce(usize, &'gc SimpleCollectorContext, &mut MakeFastaTask<'gc>) -> std::io::Result<()>>(
    mut task: MakeFastaTask<'gc1>,
    gc: &'gc1 mut SimpleCollectorContext,
    func: F
) -> std::io::Result<()> {
    task.buffer = Some(gc.alloc(RefCell::new(vec![0; BUFFER_SIZE])));
    if (task.buffer.as_ref().unwrap().borrow().len() % (LINE_LENGTH + 1)) != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "buffer size must be a multiple of line length"
        ));
    }
    let desc_string = gc.alloc(format!(">{} {}\n", &**task.id, &**task.desc));
    std::io::stdout().write_all(desc_string.as_bytes())?;
    task = safepoint!(gc, task);
    while task.n_chars > 0 {
        let chunk_size = if task.n_chars > LINE_LENGTH { LINE_LENGTH } else { task.n_chars };
        if task.buffer_index == BUFFER_SIZE {
            let buffer = task.buffer.as_ref().unwrap().borrow();
            std::io::stdout().write_all(&buffer[0..task.buffer_index])?;
            task.buffer_index = 0;
        }
        let (new_task, res) = safepoint_recurse!(gc, task, |gc, task| {
            let mut task = task;
            func(chunk_size, gc, &mut task)
        });
        match res {
            Ok(()) => {},
            Err(e) => return Err(e)
        }
        task = safepoint!(gc, new_task);
    }
    {
        let buffer = task.buffer.as_ref().unwrap().borrow();
        std::io::stdout().write_all(&buffer[0..task.buffer_index])?;
        task.buffer_index = 0;
    }
    Ok(())
}

fn make_random_fasta<'gc>(
    task: MakeFastaTask<'gc>,
    gc: &'gc mut SimpleCollectorContext,
    fpf: Gc<'gc, FloatProbFreq<'gc>>,
) -> std::io::Result<()> {
    make_fasta(task, gc, |chunk_size, gc, task| {
        let mut buffer = task.buffer.as_ref().unwrap().borrow_mut();
        task.buffer_index = fpf.select_random_into_buffer(
            &*task.state, &mut *buffer,
            task.buffer_index, chunk_size
        );
        buffer[task.buffer_index] = b'\n';
        task.buffer_index += 1;
        task.n_chars -= chunk_size;
        Ok(())
    })
}

fn make_repeat_fasta<'gc>(
    task: MakeFastaTask<'gc>,
    gc: &'gc mut SimpleCollectorContext,
    alu: &str
) -> std::io::Result<()> {
    let alu_bytes = alu.as_bytes();
    let mut alu_index = 0usize;
    make_fasta(task, gc, |chunk_size, gc, task| {
        let mut buffer = task.buffer.as_ref().unwrap().borrow_mut();
        for _ in 0..chunk_size {
            if alu_index == alu_bytes.len() {
                alu_index = 0;
            }
            buffer[task.buffer_index] = alu_bytes[alu_index];
            task.buffer_index += 1;
            alu_index += 1;
        }
        buffer[task.buffer_index] = b'\n';
        task.buffer_index += 1;
        task.n_chars -= chunk_size;
        Ok(())
    })
}

fn main() {
    let n = std::env::args().nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(1000);

    let plain = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let logger = Logger::root(
        slog_term::FullFormat::new(plain).build().fuse(),
        o!("bench" => file!())
    );
    let collector = SimpleCollector::with_logger(logger);
    let mut gc = collector.into_context();
    let mut state = gc.alloc(State::new());
    {
        let (new_state, ()) = safepoint_recurse!(gc, state, |gc, state| {
            let task = MakeFastaTask::new(&gc, state, "ONE", "Homo sapiens alu", n * 2);
            safepoint_recurse!(gc, task, |gc, task| {
                make_repeat_fasta(task, gc, ALU).unwrap();
            });
        });
        state = new_state;
    }
    state = safepoint!(gc, state);
    {
        const PROBS: &[f64] = &[0.27, 0.12, 0.12, 0.27,
            0.02, 0.02, 0.02, 0.02,
            0.02, 0.02, 0.02, 0.02,
            0.02, 0.02, 0.02];
        let (new_state, ()) = safepoint_recurse!(gc, state, |gc, state| {
            let task = MakeFastaTask::new(
                &gc, state, "ONE", "Homo sapiens alu",
                n * 2,
            );
            let iub = gc.alloc(FloatProbFreq {
                chars: gc.alloc(b"acgtBDHKMNRSVWY".iter().cloned().map(Cell::new).collect()),
                probs: gc.alloc(PROBS.iter().map(|&f| f as f32).map(Cell::new).collect())
            });
            make_random_fasta(
                task,
                gc,
                iub
            ).unwrap();
        });
        state = new_state;
    }
    state = safepoint!(gc, state);
    {
        const PROBS: &[f64] = &[0.3029549426680,
            0.1979883004921,
            0.1975473066391,
            0.3015094502008];
        let (new_state, ()) = safepoint_recurse!(gc, state, |gc, state| {
            let task =  MakeFastaTask::new(
                &gc, state, "THREE", "Homo sapiens frequency",
                n * 5,
            );
            let homo_sapiens = gc.alloc(FloatProbFreq {
                chars: gc.alloc(b"acgt".iter().cloned().map(Cell::new).collect()),
                probs: gc.alloc(PROBS.iter().map(|&f| f as f32).map(Cell::new).collect())
            });
            safepoint_recurse!(gc, (task, homo_sapiens), |gc, roots| {
                let (task, homo_sapiens) = roots;
                make_random_fasta(task, gc, homo_sapiens).unwrap();
            });
        });
        state = new_state;
    }
    state = safepoint!(gc, state);
    std::io::stdout().flush().unwrap();
}