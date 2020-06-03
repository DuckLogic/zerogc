#![feature(
    arbitrary_self_types, // Unfortunately this is required for methods on Gc refs
)]
use zerogc::{
    safepoint, safepoint_recurse, freeze_safepoint, unfreeze,
    GcSimpleAlloc, GcCell, GcSafe
};

use zerogc_simple::{SimpleCollector, SimpleCollectorContext, Gc};
use zerogc_derive::Trace;

use rayon::prelude::*;
use std::cell::RefCell;

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
    collector: &SimpleCollector,
    depth: i32, iterations: u32
) -> String {
    /*
     * Technically these are never dropped,
     * so after we're done working we can't GC again.
     *
     * This is pretty horrible
     */
    thread_local! {
        static GC: RefCell<Option<SimpleCollectorContext>> = RefCell::new(None);
    }
    let chk: i32 = (0 .. iterations).into_par_iter().map(|_| {
        GC.with(|r| {
            let mut r = r.borrow_mut();
            let gc = r.get_or_insert_with(|| {
                collector.create_context()
            });
            safepoint_recurse!(gc, |gc, new_root| {
                let () = new_root;
                let a = bottom_up_tree(&gc, depth);
                item_check(&a)
            })
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

    let collector = SimpleCollector::create();
    let mut gc = collector.create_context();
    {
        let depth = max_depth + 1;
        let tree = bottom_up_tree(&gc, depth);
        println!("stretch tree of depth {}\t check: {}", depth, item_check(&tree));
    }
    safepoint!(gc, ());

    let long_lived_tree = bottom_up_tree(&gc, max_depth);
    let (frozen, long_lived_tree) = freeze_safepoint!(gc, long_lived_tree);

    (min_depth / 2..max_depth / 2 + 1).into_par_iter().for_each(|half_depth| {
        let depth = half_depth * 2;
        let iterations = 1 << ((max_depth - depth + min_depth) as u32);
        // NOTE: We're relying on inner to do safe points internally
        let message = inner(&collector, depth, iterations);
        println!("{}", message);
    });
    unfreeze!(frozen => new_context, long_lived_tree);

    println!("long lived tree of depth {}\t check: {}", max_depth, item_check(&long_lived_tree));
}