#![feature(
    arbitrary_self_types, // Unfortunately this is required for methods on Gc refs
)]
use zerogc::{safepoint, safepoint_recurse, GcSimpleAlloc, GcCell, GcSafe};

use zerogc_simple::{SimpleCollector, SimpleCollectorContext, Gc};
use zerogc_derive::Trace;
use slog::{Logger, Drain, o};

#[derive(Trace)]
struct Tree<'gc> {
    #[zerogc(mutable(public))]
    children: GcCell<Option<(Gc<'gc, Tree<'gc>>, Gc<'gc, Tree<'gc>>)>>,
}

fn item_check(tree: &Tree) -> i32 {
    if let Some((left, right)) = tree.children.get() {
        1 + item_check(&right) + item_check(&left)
    } else {
        1
    }
}

fn bottom_up_tree<'gc>(collector: &'gc SimpleCollectorContext, depth: i32)
                       -> Gc<'gc, Tree<'gc>> {
    let tree = collector.alloc(Tree { children: GcCell::new(None) });
    if depth > 0 {
        let right = bottom_up_tree(collector, depth - 1);
        let left = bottom_up_tree(collector, depth - 1);
        tree.set_children(Some((left, right)));
    }
    tree
}

fn inner(
    gc: &mut SimpleCollectorContext,
    depth: i32, iterations: u32
) -> String {
    let chk: i32 = (0 .. iterations).into_iter().map(|_| {
        safepoint_recurse!(gc, |gc| {
            let a = bottom_up_tree(&gc, depth);
            item_check(&a)
        })
    }).sum();
    format!("{}\t trees of depth {}\t check: {}", iterations, depth, chk)
}

fn main() {
    let n = std::env::args().nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(10);
    let min_depth = 4;
    let max_depth = if min_depth + 2 > n { min_depth + 2 } else { n };

    let plain = slog_term::PlainSyncDecorator::new(std::io::stdout());
    let logger = Logger::root(
        slog_term::FullFormat::new(plain).build().fuse(),
        o!("bench" => file!())
    );
    let collector = SimpleCollector::with_logger(logger);
    let mut gc = collector.create_context();
    {
        let depth = max_depth + 1;
        let tree = bottom_up_tree(&gc, depth);
        println!("stretch tree of depth {}\t check: {}", depth, item_check(&tree));
    }
    safepoint!(gc, ());

    let long_lived_tree = bottom_up_tree(&gc, max_depth);

    let (long_lived_tree, ()) = safepoint_recurse!(gc, long_lived_tree, |gc, long_lived_tree| {
        (min_depth / 2..max_depth / 2 + 1).into_iter().for_each(|half_depth| {
            let depth = half_depth * 2;
            let iterations = 1 << ((max_depth - depth + min_depth) as u32);
            let message = safepoint_recurse!(gc, |new_gc| {
                inner(&mut new_gc, depth, iterations)
            });
            println!("{}", message);
        })
    });

    println!("long lived tree of depth {}\t check: {}", max_depth, item_check(&long_lived_tree));
}