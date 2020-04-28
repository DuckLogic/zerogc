use zerogc::{safepoint, GarbageCollectionSystem, GarbageCollected, Gc, GcConfig, GcCell};
use zerogc::safepoints::{GcErase, GcUnErase};

struct Tree<'gc> {
    children: GcCell<Option<(Gc<'gc, Tree<'gc>>, Gc<'gc, Tree<'gc>>)>>,
}
// TODO: Auto-derive
unsafe impl<'gc> GarbageCollected for Tree<'gc> {
    const NEEDS_TRACE: bool = true;

    #[inline]
    unsafe fn raw_trace(&self, collector: &mut GarbageCollectionSystem) {
        collector.trace(&self.children);
    }
}
unsafe impl<'gc, 'unm> GcErase<'unm> for Tree<'gc> {
    type Erased = Tree<'unm>;

    #[inline]
    unsafe fn erase(self) -> Self::Erased {
        std::mem::transmute(self)
    }
}
unsafe impl<'gc> GcUnErase<'static, 'gc> for Tree<'static> {
    type Corrected = Tree<'static>;

    #[inline]
    unsafe fn unerase(self) -> Self::Corrected {
        std::mem::transmute(self)
    }
}

fn item_check(tree: &Tree) -> i32 {
    if let Some((left, right)) = tree.children.get() {
        1 + item_check(&right) + item_check(&left)
    } else {
        1
    }
}

fn bottom_up_tree<'gc>(collector: &'gc GarbageCollectionSystem, depth: i32)
                       -> Gc<'gc, Tree<'gc>> {
    let mut tree = collector.alloc(Tree { children: GcCell::new(None) });
    if depth > 0 {
        let right = bottom_up_tree(collector, depth - 1);
        let left = bottom_up_tree(collector, depth - 1);
        tree.children.set(Some((left, right)));
    }
    tree
}

fn inner<'gc>(
    collector: &GarbageCollectionSystem,
    depth: i32, iterations: u32
) -> String {
    let chk: i32 = (0 .. iterations).into_iter().map(|_| {
        let a = bottom_up_tree(&collector, depth);
        item_check(&a)
    }).sum();
    format!("{}\t trees of depth {}\t check: {}", iterations, depth, chk)
}

fn main() {
    let n = std::env::args().nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(10);
    let min_depth = 4;
    let max_depth = if min_depth + 2 > n { min_depth + 2 } else { n };

    let mut collector = GarbageCollectionSystem::new(GcConfig::default());
    {
        let depth = max_depth + 1;
        let tree = bottom_up_tree(&collector, depth);
        println!("stretch tree of depth {}\t check: {}", depth, item_check(&tree));
    }
    safepoint!(collector);

    let long_lived_tree = bottom_up_tree(&collector, max_depth);

    let mut long_lived_tree = safepoint!(collector, long_lived_tree);

    let messages = (min_depth/2..max_depth/2 + 1).into_iter().map(|half_depth| {
        let depth = half_depth * 2;
        let iterations = 1 << ((max_depth - depth + min_depth) as u32);
        // TODO: Don't free the collector on inner allocations
        let msg = inner(&collector, depth, iterations);
        let traced_tree = safepoint!(collector, long_lived_tree);
        long_lived_tree = traced_tree;
        msg
    }).collect::<Vec<_>>();

    for message in messages {
        println!("{}", message);
    }

    println!("long lived tree of depth {}\t check: {}", max_depth, item_check(&long_lived_tree));
}