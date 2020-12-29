//! Utilities for the context library
//!
//! Also used by some collector implementations.
use std::fmt::{self, Debug, Formatter, Display};
#[cfg(not(feature = "sync"))]
use std::cell::Cell;

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
    Enabled {
        id: std::thread::ThreadId,
        name: Option<String>
    }
}
impl ThreadId {
    pub fn current() -> ThreadId {
        let thread = std::thread::current();
        ThreadId::Enabled {
            id: thread.id(),
            name: thread.name().map(String::from)
        }
    }
}
impl slog::Value for ThreadId {
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
            ThreadId::Enabled { id, name: None } => {
                write!(f, "{:?}", id)
            },
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
