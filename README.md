ZeroGc
=======
[WIP] Zero overhead tracing garbage collection for rust.

This is extremly experimental. I hope I will able to use it in production,
but for but for now it will probably just explode.

Instead of requiring compiler support to track.

It uses Rust's lifetime system to ensure that freed garbage isn't accessed after a collection.

This is an implementation-agnostic API. Right now there is no working implementation.

I eventually plan to implement several collection strategies (including a simple mark/compact and a fast generational one).

## Planned Features
1. Easy to use, since `Gc<T>` is `Copy` and coerces to a reference.
2. Absolutely zero overhead when modifying pointers, since `Gc<T>` is `Copy`.
3. Support for important libraries builtin to the collector
4. Unsafe code has complete freedom to manipulate garbage collected pointers, and it doesn't need to understand the distinction 
5. Uses rust's lifetime system to ensure all roots are known at explicit safepoints, without any runtime overhead.
6. Collection can only happen with an explicit `safepoint` call and has no overhead between these calls,
7. Optional graceful handling of allocation failures.

## [Motivation](Motivation.md)
The idea behind this collector that by making all potential for collections explicit,
we can take advantage of the borrow checker to statically guarantee everything's valid.
I was originally inspired to create a safe abstraction for garbage collection by [rust gc](https://github.com/Manishearth/rust-gc) by @Manishearth,
but wanted it to have zero runtime overhead while remaining very simple.\
The problem that the original collector by @Manishearth solves is finding the 'roots' of garbage collection.
His collector uses runtime tracking to maintain a reference to every GC object.

Our collector only has runtime overhead at specific safepoints,
when the shadow-stack is being updated.

Not only is this faster than runtime tracking of every single pointer or conservative collection, but it is more flexible. 
It paves the way for any combination of generational, incremental, and copying garbage collection.

## Disadvantages
1. The garbage collector can only run in response to an explicit `safepoint!`, not memory pressure,
   - This is a fundamental design limitation.
   - One can think of this as a feature, since garbage collection can be restricted to specific times and places.
   - The user must be liberal about inserting safepoins
     - Generally high-level languages which use GCs automatically insert calls to safe points
     - Their safepoints tend to be lower-overhead than ours, so you need to balance the cost/benefit
2. Unfortunately, implementing `GcSafe` for a type prevents it from having a explicit `Drop` implementation.
   - This is needed to ensure that the destructors don't do bad things, since we don't want to deal with finalizers.
   - Of course unsafe code isn't bound by this restriction, since it's assumed to behave properly
