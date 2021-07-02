#![feature(
    arbitrary_self_types, // Unfortunately this is required for methods on Gc refs
)]
use zerogc::prelude::*;
use zerogc_derive::Trace;

use rayon::prelude::*;
use slog::{Logger, Drain};
use crate::{GcTest, GcSyncTest, TestContext};
use zerogc::GcHandleSystem;

#[derive(Trace)]
#[zerogc(collector_id(Id))]
pub(crate) struct Tree<'gc, Id: CollectorId> {
    #[zerogc(mutable(public))]
    children: GcCell<Option<(Gc<'gc, Tree<'gc, Id>, Id>, Gc<'gc, Tree<'gc, Id>, Id>)>>,
}

fn item_check<Id: CollectorId>(tree: &Tree<Id>) -> i32 {
    if let Some((left, right)) = tree.children.get() {
        1 + item_check(&right) + item_check(&left)
    } else {
        1
    }
}

fn bottom_up_tree<'gc, Ctx: TestContext>(collector: &'gc Ctx, depth: i32) -> Gc<'gc, Tree<'gc, Ctx::Id>, Ctx::Id> {
    let tree = collector.alloc(Tree { children: GcCell::new(None) });
    if depth > 0 {
        let right = bottom_up_tree(collector, depth - 1);
        let left = bottom_up_tree(collector, depth - 1);
        tree.set_children(Some((left, right)));
    }
    tree
}

fn inner<GC: GcSyncTest>(
    collector: &GC,
    depth: i32, iterations: u32
) -> String {
    let chk: i32 = (0 .. iterations).into_par_iter().map(|_| {
        let mut gc = collector.create_context();
        safepoint_recurse!(gc, |gc| {
            let a = bottom_up_tree(&*gc, depth);
            item_check(&a)
        })
    }).sum();
    format!("{}\t trees of depth {}\t check: {}", iterations, depth, chk)
}

pub const EXAMPLE_NAME: &str = file!();
pub fn main<GC: GcSyncTest>(logger: &Logger, collector: GC, args: &[&str]) -> anyhow::Result<()> {
    let n = args.get(0)
        .and_then(|n| n.parse().ok())
        .unwrap_or(10);
    let min_depth = 4;
    let max_depth = if min_depth + 2 > n { min_depth + 2 } else { n };

    let mut gc = collector.create_context();
    {
        let depth = max_depth + 1;
        let tree = bottom_up_tree(&gc, depth);
        println!("stretch tree of depth {}\t check: {}", depth, item_check(&tree));
    }
    safepoint!(gc, ());

    let long_lived_tree = bottom_up_tree(&gc, max_depth);
    let long_lived_tree = GC::HandleSystem::<'_, '_, _>::create_handle(long_lived_tree);
    let frozen = freeze_context!(gc);

    (min_depth / 2..max_depth / 2 + 1).into_par_iter().for_each(|half_depth| {
        let depth = half_depth * 2;
        let iterations = 1 << ((max_depth - depth + min_depth) as u32);
        // NOTE: We're relying on inner to do safe points internally
        let message = inner(&collector, depth, iterations);
        println!("{}", message);
    });
    let new_context = unfreeze_context!(frozen);
    let long_lived_tree = long_lived_tree.bind_to(&new_context);

    println!("long lived tree of depth {}\t check: {}", max_depth, item_check(&long_lived_tree));
}