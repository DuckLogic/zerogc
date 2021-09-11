//! Utilities for the context library
//!
//! Also used by some collector implementations.
use core::fmt::{self, Debug, Formatter, Display};
use core::mem;
#[cfg(not(feature = "sync"))]
use core::cell::Cell;

/// Get the offset of the specified field within a structure
#[macro_export]
macro_rules! field_offset {
    ($target:ty, $($field:ident).+) => {{
        const OFFSET: usize = {
            let uninit = core::mem::MaybeUninit::<$target>::uninit();
            unsafe { ((core::ptr::addr_of!((*uninit.as_ptr())$(.$field)*)) as *const u8)
                .offset_from(uninit.as_ptr() as *const u8) as usize }
        };
        OFFSET
    }};
}


/// Transmute between two types,
/// without verifying that there sizes are the same
///
/// ## Safety
/// This function has undefined behavior if `T` and `U`
/// have different sizes.
///
/// It also has undefined behavior whenever [mem::transmute] has
/// undefined behavior.
#[inline]
pub unsafe fn transmute_mismatched<T, U>(src: T) -> U {
    // NOTE: This assert has zero cost when monomorphized
    assert_eq!(mem::size_of::<T>(), mem::size_of::<U>());
    let d = mem::ManuallyDrop::new(src);
    mem::transmute_copy::<T, U>(&*d)
}

#[cfg(feature = "sync")]
pub type AtomicCell<T> = ::crossbeam_utils::atomic::AtomicCell<T>;
/// Fallback `AtomicCell` implementation when we actually
/// don't care about thread safety
#[cfg(not(feature = "sync"))]
#[derive(Default)]
pub struct AtomicCell<T>(Cell<T>);
#[cfg(not(feature = "sync"))]
impl<T: Copy> AtomicCell<T> {
    pub const fn new(value: T) -> Self {
        AtomicCell(Cell::new(value))
    }
    pub fn store(&self, value: T) {
        self.0.set(value)
    }
    pub fn load(&self) -> T {
        self.0.get()
    }
    pub fn compare_exchange(&self, expected: T, updated: T) -> Result<T, T>
        where T: PartialEq {
        let existing = self.0.get();
        if existing == expected {
            self.0.set(updated);
            Ok(existing)
        } else {
            Err(existing)
        }
    }
}

#[derive(Clone)]
pub enum ThreadId {
    #[allow(unused)]
    Nop,
    #[cfg(feature = "std")]
    Enabled {
        id: std::thread::ThreadId,
        name: Option<String>
    }
}
impl ThreadId {
    #[cfg(feature = "std")]
    pub fn current() -> ThreadId {
        // NOTE: It's okay: `sync` requires std
        let thread = std::thread::current();
        ThreadId::Enabled {
            id: thread.id(),
            name: thread.name().map(String::from)
        }
    }
    #[cfg(not(feature = "std"))]
    #[inline]
    pub fn current() -> ThreadId {
        ThreadId::Nop
    }
}
impl slog::Value for ThreadId {
    #[cfg(not(feature = "std"))]
    fn serialize(
        &self, _record: &slog::Record,
        _key: &'static str,
        _serializer: &mut dyn slog::Serializer
    ) -> slog::Result<()> {
        Ok(()) // Nop
    }
    #[cfg(feature = "std")]
    fn serialize(
        &self, _record: &slog::Record,
        key: &'static str,
        serializer: &mut dyn slog::Serializer
    ) -> slog::Result<()> {
        let (id, name) = match *self {
            ThreadId::Nop => return Ok(()),
            ThreadId::Enabled { ref id, ref name } => (id, name)
        };
        match *name {
            Some(ref name) => {
                serializer.emit_arguments(key, &format_args!(
                    "{}: {:?}", *name, id
                ))
            },
            None => {
                serializer.emit_arguments(key, &format_args!(
                    "{:?}", id
                ))
            },
        }
    }
}
impl Debug for ThreadId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            ThreadId::Nop => f.write_str("ThreadId(??)"),
            #[cfg(feature = "std")]
            ThreadId::Enabled { id, name: None } => {
                write!(f, "{:?}", id)
            },
            #[cfg(feature = "std")]
            ThreadId::Enabled { id, name: Some(ref name) } => {
                f.debug_tuple("ThreadId")
                    .field(&id)
                    .field(name)
                    .finish()
            }
        }
    }
}
/// The size of memory in bytes
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct MemorySize {
    pub bytes: usize
}
impl Display for MemorySize {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            write!(f, "{}", self.bytes)
        } else {
            // Write approximation
            let bytes = self.bytes;
            let (amount, suffix) = if bytes > 1024 * 1024 * 1024 {
                (1024 * 1024 * 1024, "GB")
            } else if bytes > 1024 * 1024 {
                (1024 * 1024, "MB")
            } else if bytes > 1024 {
                (1024, "KB")
            } else {
                (1, "")
            };
            write!(f, "{:.2}{}", bytes as f64 / amount as f64, suffix)
        }
    }
}
