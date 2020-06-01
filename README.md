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
in the way it uses the rust borrow checker. It seems to be sound,
but I have no way of really knowing for sure.

The simple mark/sweep collector passes basic tests,
but it still relies on a lot of unsafe code internally.

Eventually I plan to use this in a language VM,
so it needs to be very flexible! I want to support both simple
and complex collectors with the same API.

There was previously a copying collector (which worked) 511be539228e7142,
but I removed it due to high memory usage.

## Motivation
I was originally inspired to create a safe abstraction for garbage collection by [rust gc](https://github.com/Manishearth/rust-gc)
but I wanted it to have zero runtime overhead and pointers that are `Copy`.

The main problem that the rust-gc solves is finding the 'roots' of garbage collection.
His collector uses runtime tracking to maintain a reference to every GC object.

I'm familiar with some JIT implementations, and I know they tend to use safepoints
to explicitly control where garbage collections can happen.
Normally this is an unsafe operation. All live references on the stack must
be given on the stack or use after.
It would be unsafe to trust the user to .
Eventually I realized I could use the borrow checker to restrict
the usage of roots around safepoints. I was inspired in part by the way
[indexing](https://github.com/bluss/indexing) uses lifetimes to enforce
the validity of indexes.

Our collector only has runtime overhead at specific safepoints,
when the shadow-stack is being updated.

Not only is this faster than runtime tracking of every single pointer or conservative collection, but it is more flexible. 
It paves the way for any combination of generational, incremental, and copying garbage collection.

### Prior Art
Since the original [rust gc](https://github.com/Manishearth/rust-gc) others
have attempted to make zero-overhead collectors.
I was not aware of this when I started this project.
1. [shifgrethor](https://github.com/withoutboats/shifgrethor)
  - This is **extremely** impressive. It appears to be the only other collector
    where `Gc: Copy` and coerces to a reference.
  - However, the collectors have to support pinning of roots (which we do not)
  - See this [blog post series for more](https://boats.gitlab.io/blog/post/shifgrethor-i/)
2. [cell-gc](https://github.com/jorendorff/cell-gc)
  - Unfortunately this has a rather clunky interface to Rust code,
    so it wouldn't be suitable .
  - They implement a basic List VM as a proof of concept.
3. [gc-arena](https://github.com/kyren/gc-arena)
  - Ironically the original use case for this 
    collector was a replacement for arena allocation
  - Like our collector, safepoints are explicitly requested by the user
  - However instead of attempting to rebind lifetimes,
    they attempt to use futures to build state machines

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
