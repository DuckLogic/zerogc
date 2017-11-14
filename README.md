ZeroGc
=======
Zero overhead tracing garbage collection for rust, by abusing the borrow checker.

## Idea
The idea behind this collector that by making all potential for collections explicit,
we can take advantage of the borrow checker to statically guarantee everything's valid.
We call these potential collections 'safepoints' and the borrow checker can statically prove all uses are valid in between.
There is no chance for collection to happen in between,
and you have unrestricted use of garbage collected pointers until you reach the safepoint.
Once you decide to perform a safepoint with the `safepoint!` macro,
you explicitly decide what objects you want to survive the potential collection.


## Status
- This collector design should already be completely sound, and already have a very low overhead interface
  - However, _I only think_ that my approach is sound, and someone might eventually point out a fatal flaw (however unlikely).
- Certain popular libraries are 'blessed' and have garbage collection support include
  - Although this favors popular libraries, 
  - This is only to speed adoption of the collector, not because I hate 
  - It should be quite easy to add support 
  - 
- Although there's a ton of documentation for everything, we really need a guide for new users.
  - The documentation is somewhat long and in depth, and isn't aimed at beginners.
  - As Mark Twain famously said "I would have written a shorter letter, but I did not have the time"
- **Beware, construction is in progress!**
  - The above advertisements all assume this is a good idea to run in production, which it isn't (yet).
    - Technically 
- Currently functions properly, but there are likely plenty of bugs since there's a ton of complicated unsafe code.
- The API is really complex, but it never requires any unsafe code.
- There aren't many benchmarks done,
  - Personally, i'm just happy to have a safe zero-overhead garbage collection abstraction
- Allocation isn't very optimized, and is actually slightly slower than `malloc`.
  - Copying collection should be possible in the future, which would enable bump-pointer allocation like Java.
- Many of the internals that need to be used by macros are hidden from documentation,
- The heuristics are currently somewhat sloppy.
- Unfortunately there aren't very many unit tests, though I'll add more as the project matures.
- There are some small soundness holes in the current implementation, but they should be easy to fix.
  - The only currently known problem is the _unenforced rule_ that garbage collected types can't have explicit destructors.
  - This is the reason why I require a nightly compiler,
    so I can eventually write a compiler plugin to enforce these rules.

## Features
- Remember, this is [experimental software](#Status)
  - However, the upmost care is still taken to [maintain safety](#Safety)
- Easy to use, since `Gc<T>` is `Copy` and coerces to a reference.
 - The borrow checker guarentees the lifetime of garbage collected references will be valid between `safepoint`s.
   - The lifetime of these pointers are predictable, and only need extra work at a `safepoint`
   - Unsafe code has no need to realize the memory is garbage collected, 
- Tracing implementations are already included in the collector for some important third-party libraries and the standard library
  - This includes support for `petgraph`, `ordermap`, `ndarray`, and `smallvec`
  - Many of my personal libraries include support themselves, as they're often not important enough for .
  - However if your favorite library doesn't (or won't) include support for garbage collection,
    don't despair, as it is still _safely supported to use garbage collected objects with unsupported libraries_,.
    - This is because the borrow checker verifies that only objects that support garbage collection,
      are traced by the collector are permitted to survive safepoints.
    - This means _types that don't support tracing temporarily prevent garbage collection_.
      - This is because no way for the garbage collector to collect it during a safepoint,
         so it simply refuses to allow them to cross safepoints in the first place.
- Unsafe code has complete freedom to manipulate garbage collected pointers, and it doesn't need to understand the distinction 
   between garbage collected pointers and regular pointers.
  - This is because the borrow checker demands all roots be statically known during safepoints (as described above),
    statically verifying that unsafe code from encountering unexpected garbage collection unless they explicitly claim support for it.
    - Again, you are still _safely permitted to use garbage collected pointers with any unsupported unsafe code_, 
       as long as you finish everything before you attempt to perform a safepoint.
       - The only feature they loose
       - We use the borrow checker's rules to make sure garbage collection is safe,
         and since unsafe code is already required to follow these rules,
         it automatically works with our garbage collection scheme.
  - The downside of this approach is that until unsafe code decides to support tracing,
    the user isn't able to perform garbage collection without completely dropping your structures.
    - Although support is included for some critically important libraries and the stdlib,
      more obscure third-party libraries will need to decide to support tracing themselves.
    - Even if an implementation isn't directly included it's _still perfectly acceptable to use garbage-collection unaware libraries_,
      as long as you limit all uses to in-between safepoints.
  - We export some unsafe macros to automatically provide the most common unsafe implementations,
    which should are sufficient to cover 99%
     - To be clearer about how useful these unsafe macros are,
       they're broad enough to provide implementations for the code of almost all third-party libraries.
     - The only types whose use-cases aren't covered are `petgraph` (complex graphs) and the garbage collector itself
     - Usually you only need to invoke either the `unsafe_trace_iterable!` or `unsafe_trace_primitive!` macros,
       in order to fully support garbage collection .      
       - Again _most unsafe code only needs to invoke a macro_ in order to fully support garbage collection.
     - Safe code can already use the completely safe automatically derived implementations to support garbage collection,
       so there is absolutely zero unsafe code (or macro invocations) involved. 
- Absolutely zero overhead when modifying pointers, since `Gc<T>` is `Copy`.
  - This gives a huge throughput advantage over reference counting.
    - This has been independently verified by multiple benchmarks
  - You can manipulate pointers nonstop without overhead,
    then decide to call a `safepoint` only when it's good to stop 
- Uses rust's lifetime system to ensure all roots are known at explicit safepoints, without any runtime overhead.
  - zerogc's explicit safepoint abstraction would be wildly unsafe in any other language, but in rust it's completely safe.
  - This solves the problem of finding the roots by
    putting the burden on the user to list them at explicit safealpoints.
  - Java and C# already list the roots using implicit safepoints, lowing garbage collection to happen implicitly.
    - zerogc simply makes these safepoints explicit and requires the user to explicitly state them.
      - Not only is this more predictable, but it falls in line with the rust philosophy to overhead explicit.
- Collection can only happen with an explicit `safepoint` call and has no overhead between these calls,
   giving real time guarantees not possible with fully automatic garbage collection.
  - Even better, rust's safety system guarantees that garbage collected pointers
    can't be in another thread during the `safepoint` call,
    so garbage collection only needs to stop a single thread at a time.
  - Thanks to taking advantage of rust's statically provable lifetime guarantees,
    we can make all of this logic is incredibly lightweight compared to true, multithreaded, STW garbage collection algorithms.
- Optional graceful handling of allocation failures.
  - By using `try_alloc`, you can handle allocation failures with a `GcMemoryError`
  - This is perfect when building a garbage collected language,
    or if you want to have potential.
  - Ironically this gives you _more control over OOM_ then with malloc/free,
    since handling out of memory errors currently isn't stable or safe.
  - In fact `alloc` is simply a wrapper around `try_alloc` that panics on failure

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
- The garbage collector can only run in response to a `safepoint`, not memory pressure,
  - This is a fundamental design limitation.
  - The user must be liberal about inserting safepoints, always inserting
- You must explicitly pass roots to a `safepoint`.
- Unfortunately, deriving `GarbageCollected` for a type prevents it from having a explicit `Drop` implementation.
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

## How it works
We prevent all garbage collection until the user explicitly allows it with a safepoint.
When the user requests a safepoint, they explicitly list all the garbage collected objects they want to survive.
The collector decides if collection whether or not is needed,
and may decide to perform a traditional mark-sweep collection (using the collections as root).

The borrow checker considers the safepoint a mutation,
and rust's lifetime rules statically guarantee that all garbage collected pointers are invalid after this point.
However, an explicit exception is made for the objects explicitly passed to the safepoint,
since they'll be traced by the garbage collector and will be considered the garbage collector's roots.
This system statically guarantees that all pointers are known to the collector,
and that the collector has been explicitly requested by the user,
without any compiler modification or unsafe code on the user's behalf.
