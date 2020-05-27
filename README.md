ZeroGc
=======
[WIP] Zero overhead tracing garbage collection for rust.


## Planned Features
1. Easy to use, since `Gc<T>` is `Copy` and coerces to a reference.
2. Absolutely zero overhead when modifying pointers, since `Gc<T>` is `Copy`.
3. Support for important libraries builtin to the collector
4. Implementation agnostic API
5. Unsafe code has complete freedom to manipulate garbage collected pointers, and it doesn't need to understand the distinction 
6. Uses rust's lifetime system to ensure all roots are known at explicit safepoints, without any runtime overhead.
7. Collection can only happen with an explicit `safepoint` call and has no overhead between these calls,
8. API supports moving objects (allowing copying/generational GCs)

Instead of requiring compiler support to track GC roots (like Java/Go),
it uses a shadow stack to keep track of GC roots.
Collections can only happen at explicit safepoint.

It uses Rust's lifetime system to ensure that freed garbage
isn't accessed after a collection. Allocated objects are tied
to the lifetime of the garbage collector.
A safepoint (potential collection) is treated as a mutation by
the borrow checker. Without zerogc (using a Vec or typed-arena),
this would invalidate all previously allocated pointers. However,
the any 'roots' given to the safepoint are rebound to the new lifetime
(via dark magic).

This API aims to be implementation-agnostic,
simply defining the `Trace` trait and the interface for safepoints.

Right now the only implementation is `zerogc-simple`,
which is a basic mark-sweep collector.
It's relatively fast and lightweight, making it a good default.
It uses fast arena allocation for small objects (optional; on by default) and
falls back to the system allocator for everything else.

In spite of the mark/sweep collector's simplicity,
it's reasonably competitive with other languages: b73c9828e4066 774106545cb4d06b

The library is mostly undocumented, since I expect it to change significantly in the future.
See the [binary-trees](libs/simple/examples/binary_trees.rs) example for a basic sample.

## Status
This is extremely experimental. It's really uncharted territory
in the way it uses the rust garbage collector. It seems to be sound,
but I have no way of really knowing for sure.

The simple mark/sweep collector passes basic tests,
but it still relies on a lot of unsafe code internally.

Eventually I plan to use this in a language VM,
so it needs to be very flexible! I want to support both simple
and complex collectors with the same API.

There was previously a copying collector (which worked) 511be539228e7142,
but I removed it due to high memory usage.

## Motivation
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
