The idea behind this collector that by making all potential for collections explicit,
we can take advantage of the borrow checker to statically guarantee everything's valid.
I was originally inspired to create a safe abstraction for garbage collection by [rust gc](https://github.com/Manishearth/rust-gc) by @Manishearth,
but wanted it to have zero runtime overhead while remaining very simple.\
The problem that the original collector by @Manishearth solves is finding the 'roots' of garbage collection.
His collector uses runtime tracking to maintain the roots (slow),
but my collector.
Not only is this faster than runtime tracking or forcible garbage collection, but it is much more flexible,
and paves the way for any combination of generational, incremental, and copying garbage collection.

However, in that glorious futrure with generational and incremental collectors as a feature,
it will be completely optional and you can just flip a switch to turn it on or off.
This is because a true generational or incremental collector requires write barriers,
and that would go against the philosophy of preventing runtime overhead while collecting.
However, because I want to enable write barriers in the future,
I always give out immutable references and force you to use `GcCell` to get around this.
This is because I want the user to just be able to flip a switch to turn on write barriers and incremental collection,
without having to modify their libraries or code.

