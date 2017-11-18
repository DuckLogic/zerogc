Features
========
1. Easy to use, since `Gc<T>` is `Copy` and coerces to a reference.
 1. The borrow checker guarantees the lifetime of garbage collected references will be valid between `safepoint`s.
 2. The lifetime of these pointers are predictable, and only needs extra work at a `safepoint`
 3. Unsafe code has no need to realize the memory is garbage collected, 
2. Absolutely zero overhead when modifying pointers, since `Gc<T>` is `Copy`.
  1. This gives a huge throughput advantage over reference counting.
    - This has been independently verified by multiple benchmarks
  2. You can manipulate pointers nonstop without overhead,
    then decide to call a `safepoint` only when it's good to stop 
3. Support for important libraries builtin to the collector
  1. This includes support for `serde`, `petgraph`, `ordermap`, `ndarray`, and `smallvec`
  2. However, due to the safe design _unsupported libraries don't prevent usage of the garbage collector_,
     they just _temporarily_ delay it.
   1. This is because the borrow checker verifies that only objects that support garbage collection,
  3. However means _types that don't support tracing temporarily delay garbage collection_.
3. Unsafe code has complete freedom to manipulate garbage collected pointers, and it doesn't need to understand the distinction 
   between garbage collected pointers and regular pointers.
  1. This is because the borrow checker demands all roots be statically known during safepoints (as described above),
    statically verifying that unsafe code can't encountering unexpected garbage collection unless they explicitly claim support for it.
  2. We use the borrow checker's rules to make sure garbage collection is safe,
       and unsafe code is already required to follow these rules.
  3. The downside of this approach is that until unsafe code decides to support tracing,
     that unsafe code temporarily prevents garbage collection.
   1. Although support is included for some critically important libraries and the stdlib,
      more obscure third-party libraries will need to decide to support tracing themselves.
  3. We export some unsafe macros to automatically provide the most common unsafe implementations,
    which should are sufficient to cover 99%
     - To be clearer about how useful these unsafe macros are,
       they're broad enough to provide implementations for the code of almost all third-party libraries.
     - The only types whose use-cases aren't covered are `petgraph` (complex graphs) and the garbage collector itself
     - Usually you only need to invoke either the `unsafe_trace_iterable!` or `unsafe_trace_primitive!` macros,
       in order to fully support garbage collection .      
       - Again _most unsafe code only needs to invoke a macro_ in order to fully support garbage collection.
     - Safe code can already use the completely safe automatically derived implementations to support garbage collection,
       so there is absolutely zero unsafe code (or macro invocations) involved. 
4. Uses rust's lifetime system to ensure all roots are known at explicit safepoints, without any runtime overhead.
  1. zerogc's explicit safepoint abstraction would be wildly unsafe in any other language, but in rust it's completely safe.
   1. This solves the problem of finding the roots by
     putting the burden on the user to list them at explicit safealpoints.
  2. Java and C# already list the roots using implicit safepoints, allowing garbage collection to happen implicitly.
    - zerogc simply makes these safepoints explicit and requires the user to explicitly state them.
      - Not only is this more predictable, but it falls in line with the rust philosophy to overhead explicit.
5. Collection can only happen with an explicit `safepoint` call and has no overhead between these calls,
   giving real time guarantees not possible with fully automatic garbage collection.
  1. Even better, rust's safety system guarantees that garbage collected pointers
    can't be in another thread during the `safepoint` call,
    so garbage collection only needs to stop a single thread at a time.
  2.Thanks to taking advantage of rust's statically provable lifetime guarantees,
    we can make all of this logic is incredibly lightweight compared to true,
    multithreaded, STW garbage collection algorithms.
6. Optional graceful handling of allocation failures.
  1. By using `try_alloc`, you can handle allocation failures with a `GcMemoryError`
  2. This is perfect when building a garbage collected language.
  3. Ironically this gives you _more control over OOM then malloc_,
    since failable allocation isn't stable yet.
  4. In fact `alloc` is simply a wrapper around `try_alloc` that panics on failure