Safety
======
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
