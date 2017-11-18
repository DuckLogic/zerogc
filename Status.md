Status
=======
1. This is **experimental software** made by someone **overthinking** [the general problem](https://xkcd.com/1592/)
  1. This collector design should already be completely sound, and already have a very low overhead interface
  2. However, _I only think_ that my approach is sound, and someone might eventually point out a fatal flaw (however unlikely).
2. Certain popular libraries are 'blessed' and have garbage collection support include
  1. Although this favors popular libraries, it will speed adoption of the collector
  2. It should be quite easy to add support for your own libraries,
   1. I do not favor my own libraries, and I don't include builtin support.
3. Although there's a ton of documentation for everything, we really need a user guide.
  1. As Mark Twain famously said "I would have written a shorter letter, but I did not have the time"
4. Currently functions properly, but there are likely plenty of bugs since there's a ton of complicated unsafe code.
5. There is a complex API hidden behind a macro, but absolutely no unsafe code.
  1. The API is designed to be usable even with `#[forbid(unsafe_code)]`,
     in order to ensure the soundness of my approach
  2. Many of the internals that need to be used by macros are hidden from documentation,
  1. The above advertisements all assume this is a good idea to run in production, which it isn't (yet).
6. There aren't any benchmarks done.
  1. Personally, i'm just happy to have a safe zero-cost abstraction for garbage collection.
  2. Allocation isn't very optimized, and is actually slightly slower than `malloc`.
7. Copying collection should be possible in the future
  1. This would enable bump-pointer allocation like Java.
8. The garbage collection heuristics are currently somewhat sloppy.
9. Unfortunately there aren't very many unit tests, though I'll add more as the project matures.
10. There are some _small_ soundness holes in the current _implementation_,
   but they should be easy to fix:
  1. The _unenforced rule_ that garbage collected types can't have explicit destructors.
   1. This is the reason why I require a nightly compiler,