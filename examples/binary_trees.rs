use slog::{o, Drain, Logger};
use std::cell::Cell;
use std::ptr::NonNull;
use zerogc_next::context::SingletonStatus;
use zerogc_next::{Collect, CollectContext, CollectorId, GarbageCollector, Gc, NullCollect};

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
struct ThisCollectorId;

unsafe impl CollectorId for ThisCollectorId {
    const SINGLETON: Option<SingletonStatus> = Some(SingletonStatus::Global);

    #[inline]
    unsafe fn summon_singleton() -> Option<Self> {
        Some(ThisCollectorId)
    }
}

struct Tree<'gc> {
    children: Cell<
        Option<(
            Gc<'gc, Tree<'gc>, ThisCollectorId>,
            Gc<'gc, Tree<'gc>, ThisCollectorId>,
        )>,
    >,
}

unsafe impl<'gc> Collect<ThisCollectorId> for Tree<'gc> {
    type Collected<'newgc> = Tree<'newgc>;
    const NEEDS_COLLECT: bool = true;

    unsafe fn collect_inplace(
        target: NonNull<Self>,
        context: &mut CollectContext<'_, ThisCollectorId>,
    ) {
        let mut children = target.as_ref().children.get();
        match &mut children {
            Some((left, right)) => {
                Gc::collect_inplace(NonNull::from(left), context);
                Gc::collect_inplace(NonNull::from(right), context);
            }
            None => {} // no need to traxe
        }
        target.as_ref().children.set(children);
    }
}

fn item_check(tree: &Tree) -> i32 {
    if let Some((left, right)) = tree.children.get() {
        1 + item_check(&right) + item_check(&left)
    } else {
        1
    }
}

fn bottom_up_tree<'gc>(
    collector: &'gc GarbageCollector<ThisCollectorId>,
    depth: i32,
) -> Gc<'gc, Tree<'gc>, ThisCollectorId> {
    let tree = collector.alloc(Tree {
        children: Cell::new(None),
    });
    if depth > 0 {
        let right = bottom_up_tree(collector, depth - 1);
        let left = bottom_up_tree(collector, depth - 1);
        tree.children.set(Some((left, right)));
    }
    tree
}

fn inner(gc: &mut GarbageCollector<ThisCollectorId>, depth: i32, iterations: u32) -> String {
    let chk: i32 = (0..iterations)
        .into_iter()
        .map(|_| {
            let a = bottom_up_tree(&gc, depth);
            let res = item_check(&a);
            gc.collect();
            res
        })
        .sum();
    format!("{}\t trees of depth {}\t check: {}", iterations, depth, chk)
}

fn main() {
    let n = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(10);
    let min_depth = 4;
    let max_depth = if min_depth + 2 > n { min_depth + 2 } else { n };

    let plain = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let logger = Logger::root(
        slog_term::FullFormat::new(plain).build().fuse(),
        o!("bench" => file!()),
    );
    let mut collector = unsafe { GarbageCollector::with_id(ThisCollectorId) };
    {
        let depth = max_depth + 1;
        let tree = bottom_up_tree(&collector, depth);
        println!(
            "stretch tree of depth {}\t check: {}",
            depth,
            item_check(&tree)
        );
    }
    collector.collect();

    let long_lived_tree = collector.root(bottom_up_tree(&collector, max_depth));

    {
        (min_depth / 2..max_depth / 2 + 1)
            .into_iter()
            .for_each(|half_depth| {
                let depth = half_depth * 2;
                let iterations = 1 << ((max_depth - depth + min_depth) as u32);
                let message = inner(&mut collector, depth, iterations);
                collector.collect();
                println!("{}", message);
            })
    }

    println!(
        "long lived tree of depth {}\t check: {}",
        max_depth,
        item_check(&*long_lived_tree.resolve(&collector))
    );
}
