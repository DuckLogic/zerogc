# zerogc-next (v0.3)
A **prototype** of the next version of [zerogc], a safe garbage-collector API for rust.

## Warning
**PLEASE DO NOT USE THIS CODE! IT IS A PROTOTYPE!**

Prefer the more polished implementation that will be released in v0.3 of [zerogc].

## Motivation
The API used in zerogc v0.1/v0.2 is currently known to be unsound. This is a safe replacement that successfully executes under [miri].

The design is based around "virtual copies" (a better name might be "phantom copies"), as-if types are being copied from an old lifetime `'gc` into types with a new gc lifetime `'newgc`.



While this is the API that is exposed publicly, a different `unsafe` API is used in practice to avoid the need for copies in some cases.

[miri]: https://github.com/rust-lang/miri
[zerogc]: https://github.com/DuckLogic/zerogc
