# zerogc-next (v0.3)
A safe garbage-collector API for rust.

This is intended to be replacement/redesign for the [zerogc](https://github.com/DuckLogic/zerogc) API for the 0.3 release. The API used in v0.1/v0.2 is currently known to be unsound.

It's as if types are being copied from ones with the old gc lifetime `'gc` into types with a new gc lifetime `'newgc`.

