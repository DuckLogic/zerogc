//! Support for parking-lot types
use parking_lot::{Mutex, RwLock};

use zerogc::{Trace, NullTrace};
use zerogc_derive::unsafe_gc_impl;

unsafe_trace_lock!(Mutex, target = T; |lock| lock.get_mut(), |lock| lock.lock());
unsafe_trace_lock!(RwLock, target = T; |lock| lock.get_mut(), |lock| lock.write());

