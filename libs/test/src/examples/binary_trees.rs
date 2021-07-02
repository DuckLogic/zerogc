use zerogc::prelude::*;
use zerogc::{Gc, CollectorId};
use zerogc_derive::Trace;

use slog::{Logger, Drain,};
use crate::{GcTest, TestContext};

#[derive(Trace)]
#[zerogc(collector_id(Id))]
struct Tree<'gc, Id: CollectorId> {
    #[zerogc(mutable(public))]
    children: GcCell<Option<(Gc<'gc, Tree<'gc, Id>, Id>, Gc<'gc, Tree<'gc, Id>, Id>)>>,
}

fn item_check<'gc, Id: CollectorId>(tree: &Tree<'gc, Id>) -> u32 {
    if let Some((left, right)) = tree.children.get() {
        1 + item_check(&right) + item_check(&left)
    } else {
        1
    }
}

fn bottom_up_tree<'gc, Ctx: TestContext>(collector: &'gc Ctx, depth: u32) -> Gc<'gc, Tree<'gc, Ctx::Id>, Ctx::Id> {
    let tree = collector.alloc(Tree { children: GcCell::new(None) });
    if depth > 0 {
        let right = bottom_up_tree(collector, depth - 1);
        let left = bottom_up_tree(collector, depth - 1);
        tree.set_children(Some((left, right)));
    }
    tree
}

fn inner<Ctx: TestContext>(
    gc: &mut Ctx,
    depth: u32, iterations: u32
) -> String {
    let chk: u32 = (0 .. iterations).into_iter().map(|_| {
        safepoint_recurse!(gc, |gc| {
            let a = bottom_up_tree(&*gc, depth);
            item_check(&a)
        })
    }).sum();
    format!("{}\t trees of depth {}\t check: {}", iterations, depth, chk)
}

pub const EXAMPLE_NAME: &str = file!();
pub fn main<Ctx: GcTest>(logger: &Logger, gc: Ctx, args: &[&str]) -> anyhow::Result<()> {
    let n = args.get(0)
        .map(|arg| arg.parse::<u32>().unwrap())
        .unwrap_or(10);
    assert!(n >= 0);
    let min_depth = 4;
    let max_depth = if min_depth + 2 > n { min_depth + 2 } else { n };

    let mut gc = gc.into_context();
    {
        let depth = max_depth + 1;
        let tree = bottom_up_tree(&gc, depth);
        println!("stretch tree of depth {}\t check: {}", depth, item_check(&tree));
    }
    safepoint!(gc, ());

    let long_lived_tree = bottom_up_tree(&gc, max_depth);

    let (long_lived_tree, ()) = safepoint_recurse!(gc, long_lived_tree, |gc, _long_lived_tree| {
        (min_depth / 2..max_depth / 2 + 1).into_iter().for_each(|half_depth| {
            let depth = half_depth * 2;
            let iterations = 1 << ((max_depth - depth + min_depth) as u32);
            let message = safepoint_recurse!(gc, |new_gc| {
                inner(&mut *new_gc, depth, iterations)
            });
            println!("{}", message);
        })
    });

    println!("long lived tree of depth {}\t check: {}", max_depth, item_check(&long_lived_tree));
    Ok(())
}