use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::Display;
use std::mem::ManuallyDrop;
use std::panic::Location;

mod layout_helpers;

pub use self::layout_helpers::{Alignment, LayoutExt};

enum AbortReason<M: Display> {
    Message(M),
    FailedAbort,
}

/// A RAII guard that aborts the process if the operation fails/panics.
///
/// Can be used to avoid exception safety problems.
///
/// This guard must be explicitly dropped with [`defuse`](AbortFailureGuard::defuse).
#[must_use]
pub struct AbortFailureGuard<M: Display> {
    reason: AbortReason<M>,
    location: Option<&'static Location<'static>>,
}
impl<M: Display> AbortFailureGuard<M> {
    #[inline]
    #[track_caller]
    pub fn new(reason: M) -> Self {
        AbortFailureGuard {
            reason: AbortReason::Message(reason),
            location: Some(Location::caller()),
        }
    }

    #[inline]
    pub fn defuse(mut self) {
        // replace with a dummy value and drop the real value
        drop(std::mem::replace(
            &mut self.reason,
            AbortReason::FailedAbort,
        ));
        std::mem::forget(self);
    }

    #[inline]
    pub fn fail(&self) -> ! {
        self.erase().fail_impl()
    }

    #[inline]
    fn erase(&self) -> AbortFailureGuard<&'_ dyn Display> {
        AbortFailureGuard {
            reason: match self.reason {
                AbortReason::Message(ref reason) => AbortReason::Message(reason as &'_ dyn Display),
                AbortReason::FailedAbort => AbortReason::FailedAbort,
            },
            location: self.location,
        }
    }
}
impl<'a> AbortFailureGuard<&'a dyn Display> {
    #[cold]
    #[inline(never)]
    pub fn fail_impl(&self) -> ! {
        match self.reason {
            AbortReason::Message(msg) => {
                let secondary_abort_guard = AbortFailureGuard {
                    reason: AbortReason::<std::convert::Infallible>::FailedAbort,
                    location: self.location,
                };
                eprintln!("Aborting: {msg}");
                let backtrace = Backtrace::capture();
                if let Some(location) = self.location {
                    eprintln!("Location: {location}")
                }
                if !std::thread::panicking() {
                    eprintln!(
                        "WARNING: Thread not panicking (forgot to defuse AbortFailureGuard?)"
                    );
                }
                if matches!(backtrace.status(), BacktraceStatus::Captured) {
                    eprintln!("Backtrace: {backtrace}");
                }
                secondary_abort_guard.defuse();
            }
            // don't do anything, failed to print primary abort message
            AbortReason::FailedAbort => {}
        }
        std::process::abort();
    }
}
impl<M: Display> Drop for AbortFailureGuard<M> {
    #[cold]
    #[inline]
    fn drop(&mut self) {
        self.fail()
    }
}

/// Transmute one type into another,
/// without doing compile-time checks for sizes.
///
/// The difference between [`mem::transmute`] is that this function
/// does not attempt to check sizes at compile time.
/// In the regular [`mem::transmute`] function, an compile-time error occurs
/// if the sizes can't be proved equal at compile time.
/// This function does `assert_eq!` at runtime instead.
///
/// In some contexts, the sizes of types are statically unknown,
/// so a runtime assertion is better than a static compile-time check.
///
/// ## Safety
/// See [`mem::transmute`] for full details on safety.
///
/// The sizes of the two types must exactly match,
/// but unlike [`mem::transmute`] this is not checked at compile time.
///
/// Because transmute is a by-value operation,
/// the alignment of the transmuted values themselves is not a concern.
#[inline(always)]
#[track_caller] // for the case where sizes don't match
pub unsafe fn transmute_arbitrary<Src, Dst>(val: Src) -> Dst {
    let size_matches = const { std::mem::size_of::<Src>() == std::mem::size_of::<Dst>() };
    if size_matches {
        let src: ManuallyDrop<Src> = ManuallyDrop::new(val);
        std::mem::transmute_copy(&src as &Src)
    } else {
        mismatch_transmute_sizes(
            TransmuteTypeInfo::new::<Src>(),
            TransmuteTypeInfo::new::<Dst>(),
        )
    }
}

struct TransmuteTypeInfo {
    size: usize,
    type_name: &'static str,
}
impl TransmuteTypeInfo {
    #[inline]
    pub fn new<T>() -> Self {
        TransmuteTypeInfo {
            size: std::mem::size_of::<T>(),
            type_name: std::any::type_name::<T>(),
        }
    }
}

#[cold]
#[track_caller]
fn mismatch_transmute_sizes(src: TransmuteTypeInfo, dst: TransmuteTypeInfo) -> ! {
    assert_eq!(
        src.size, dst.size,
        "Mismatched size between Src `{}` and Dst `{}`",
        src.type_name, dst.type_name
    );
    unreachable!() // sizes actually match
}
