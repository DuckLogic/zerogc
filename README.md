ZeroGc ![docs.rs]
=======
Zero overhead tracing garbage collection for rust.

## [Major Features](Features.md)
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
His collector uses runtime tracking to maintain the roots (slow),
but my collector.
Not only is this faster than runtime tracking or forcible garbage collection, but it is much more flexible,
and paves the way for any combination of generational, incremental, and copying garbage collection.

## [Status](Status.md)
1. This is **experimental software** made by someone **overthinking** [the general problem](https://xkcd.com/1592/)
2. Certain popular libraries are 'blessed' and have garbage collection support included
3. Although there's a ton of documentation for everything, we really need a user guide.
4. Currently functions properly, but there are likely plenty of bugs since there's a ton of complicated unsafe code.
5. There is a complex API hidden behind a macro, but absolutely no unsafe code.
6. There aren't any benchmarks done.
7. Copying collection should be possible in the future
8. The garbage collection heuristics are currently somewhat sloppy.
9. Unfortunately there aren't very many unit tests, though I'll add more as the project matures.
10. There are some _small_ soundness holes in the current _implementation_,
   but they should be easy to fix:


### Known soundness holes
However, there are some known (temporary) soundness holes:
1. Custom Destructors
  - It is undefined behavior for a custom destructor to reference garbage collected pointers
    - The compiler usually prevents the user from dropping items with dangling pointers
       by performing [drop check](https://doc.rust-lang.org/nightly/nomicon/dropck.html),
       however that doesn't work to enforce that the are safe for the collector to drop.
    - This is possible in entirely safe code, since custom destructors can be implemented by the user.
  - This is temporary, because I eventually intend to write a compiler plugin to perform bastardized drop check.


## Disadvantages
1. The garbage collector can only run in response to a `safepoint!`, not memory pressure,
  - This is a fundamental design limitation.
  - The user must be liberal about inserting safepoints, always inserting
2. You must either explicitly rebind the root values from a `collect!` (but not `safepoint!`).
  - In other words either do `let root = collect!(collector, root);` or `safepoint!(collector, safepoint)`  
3. Unfortunately, deriving `GarbageCollected` for a type prevents it from having a explicit `Drop` implementation.
  - This is needed to ensure that the destructors don't do bad things, since we don't want to deal with finalizers.
  - Of course unsafe code isn't bound by this restriction, since it's assumed to behave properly
- Garbage collector currently isn't generational or incremental.
  - Since rust code is unlikely to allocate millions of garbage objects a second,
    a generational collector often isn't that important.
  - Fortunately this means you don't need to have write barriers, improving performance.
- Unsafe types must manually implement `GarbageCollected` if they use raw pointers and still want to be tracedxc.
  - You can't just automatically derive the implementation, since we can't know what the heck you're doing with your pointers.
  - Implementations are provided for types found in the stdlib like `Vec` and `HashMap`
  - There are ton of ways to make a mistake, and it's very easy to trigger undefined behavior.
    - Safe code should always prefer automatic 


## Safety
All unchecked assumptions are encapsulated and hidden from the user as implementation details,
and it's impossible to break them in user code due to either runtime checks,
rust ownership rules, or strict rust encapsulation.

The absolute upmost care is taken to make sure the design of this collector is safe above all else.
I guarantee that is impossible to cause undefined behavior with the user's safe safe code, unless you trigger a bug in the implementation.
This is simply the same guarantee of the (safe subset) of the rust language as a whole,
which I took quite seriously when designing this collector.

The implementation takes the upmost steps to maintain this safety,
regardless of whatever actions safe code could possibly take.

The most delicate part of the implementation is the safepoint since that involves a user-provided object,
and tricking the borrow checker into thinking the lifetime of garbage collected pointers have changed.
However, I have taken the upmost care that the worst behavior safe code could trigger is a panic or the collector being poisoned (unusable).
For example, although the collector doesn't have explicitly check for poisoning,
For example, beginning a safepoint then invoking `mem::forget` on it irreversibly scars the collector
and prevents the user from ever starting another safepoint.
In other words the collector demands that you finish what you've started, since that's what everybody expects.

If I find a soundness hole in the API design, I will fix it even if it means yanking versions and making massive changes.
There are even tests for putting the collector in an invalid state, to ensure this is always prevented.
Another protection we have for mistakes of safe code,
is that we always verify that a garbage collector is only tracing and marking objects it owns.
That way we don't actually modify the headers of objects created by another garbage collector.
