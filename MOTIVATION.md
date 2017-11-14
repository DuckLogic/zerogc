## Motivation
ZeroGc fills a gap in the rust ecosystem for dealing with large and complicated object graphs,
since zerogc is incredibly convenient, never leaks memory, and has zero overhead when not collecting.

While you could switch to a garbage collected language in order to reap these benefits,
you lose the low-level control rust's ownership paradigm otherwise provides.
Using zerogc allows you to **choose exactly which objects are best managed by garbage collection**,
by taking advantage of rust's ownership system to enforce garbage collection constraints.
Garbage collection is not all or nothing, and most of your application's allocations will still fit perfectly into.
This prevents the problems of unessicarrily garbage collecting all objects,
putting much less load on the garbage collector,
and avoiding the latency and performance problems that plauge languages like Java with forced garbage collection.
Due to rust's ownership system enforcing thread safety **we can safely prevent all STW pauses**,
by using the same zero-cost static guarentees that keeps `Rc` safe.

Unlike [reference counted garbage collection](https://en.wikipedia.org/wiki/Reference_counting),
tracing collectors have **absolutely no overhead when manipulating pointers**,
and unreachable memory is always eventually freed, even if there are cycles.
Independent studies have confirmed that **tracing garbage collection has much higher throughput than reference counting**,
since there's no book keeping.
Garbage collected pointers are not only more performance than reference counting,
but they're more ergonomic since **garbage collected pointers can be implicitly copied** instead of .
In my application, I understood the high cost of manipulating 
The major downside of garbage collection is long latency during collections,
but zerogc only allows these collections to happen at an explicit safepoint.
This makes the latency costs always explicitly permitted by the user,
and can't unexpectedly interrupt an important operation or request.

In my application, I understood the unacceptable overhead of using reference counted pointers in complex object graphs,
so i decided to use [arena allocation](https://github.com/SimonSapin/rust-typed-arena) instead.
While arena allocation keeps many of these important performance and ease of use benefits,
and are excellent if they're quickly destroyed,
they're a number of problems for using them with complex object graphs.
The primary one is that _arena allocation irreversibly leaks memory_, and the only 

In conclusion, while Rust's usual ownership paradigms have proven sufficient to avoid garbage collection in most cases,
properly using Garbage Collection has proven itself to be the superior memory management scheme for managing complex object graphs,
and zerogc makes all the performance and latency takeoffs explicit and safe.
An application that uses zerogc's tracing garbage collection to appropriately manage the memory of complex object graphs is
only is more convenient than one that uses reference counted garbage collection, but is also significantly faster.

## Alternatives
- Use a garbage collected language like Java, Go, or Python.
  - They are _much easier to use_, since `zerogc` requires a good understanding of rust ownership rules to avoid complete insanity.
    - I think Python and Java are awesome languages, and help people quickly prototype ideas,
      but they aren't good if you need predictable performance
  - They also offer you significantly less control over your memory,
      forcing a garbage collection on you even for temporary objects.
    - With zerogc, you still retain the full control and safety that rust with the option of garbage collecting some allocations.
      - Even with `zerogc` rust is still  mostly a systems program languge,
        and most objects will continue to use `malloc` and `free`
      - Again the intent is to provide an attractive alternative to reference counting and arena allocation.
    - Although Go and C# allow value types,
      they still don't have rust's unique ownership system to ensure deterministic resource allocation
- Use reference counting
  - Reference counting is much more 'lightweight' then a garbage collector,
    and is excellent for simple objects like strings.
  - Reference counting will leak memory if there are cycles, forcing significant gymnastics to prevent leaks.
    - Although usage of weak pointers can prevent many of these leaks, it's complicated 
    - Personally, I just don't want to care whether or not memory contains cycles, and just want my garbage cleaned up.
    - Cycle collecting garbage collectors fix these problems, 
       they are heavyweight and make reference counting even slower.
  - Reference counting often has much lower throughput than garbage collection
    - Although rust somewhat avoids this overhead by making it explicit, it still isn't
- Use [rust-gc](https://github.com/Manishearth/rust-gc), which requires runtime tracking of roots.
  - The runtime tracking is even slower than reference counting.
  - Although awesome, it isn't very convenient to use and requires much more overhead.
    - Even worse is the fact that there's no one size fits all solution that merits inclusion in the compiler.
- Wait for the rust team to add true automatic garbage collection
  - This would of course require a ton of cooperation from unsafe code, much more so than zerogc
  - Previous attempts to automatically add safepoints to Rust failed due to the difficulty
    and fact that it'd need the cooperation of all unsafe code.
- Use arena allocation
  - This is great, but you can never free memory unless you destroy the entire arena.
  - Personally, I invented this out of a desire to 'clean up' in between using the arenas,
    and that's the original use case of this collector.
- Use C++ or C
  - I'm going to keep my fat mouth shut here.

