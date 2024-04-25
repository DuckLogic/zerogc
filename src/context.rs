mod young;

use crate::gcptr::Gc;
use crate::utils::LayoutExt;
use crate::{Collect, NullCollect};
use allocator_api2::alloc::Allocator;
use bitbybit::bitfield;
use bumpalo::Bump;
use std::alloc::{Layout, LayoutError};
use std::any::TypeId;
use std::cmp;
use std::fmt::Debug;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

pub enum SingletonStatus {
    /// The singleton is thread-local.
    ///
    /// This is slower to resolve,
    /// but can be assumed to be unique
    /// within the confines of an individual thread.
    ///
    /// This implies the [`CollectorId`] is `!Send`
    ThreadLocal,
    /// The singleton is global.
    ///
    /// This is faster to resolve,
    /// and can further assume to be unique
    /// across the entire program.
    Global,
}

pub unsafe trait CollectorId: Copy + Debug + Eq + 'static {
    const SINGLETON: Option<SingletonStatus>;
    unsafe fn resolve_collector(&self) -> *mut GarbageCollector<Self>;

    unsafe fn summon_singleton() -> Option<Self>;
}

pub struct GarbageCollector<Id: CollectorId> {
    allocator: Bump,
    id: Id,
}
impl<Id: CollectorId> GarbageCollector<Id> {
    #[inline]
    pub fn id(&self) -> Id {
        self.id
    }

    #[inline(always)]
    pub fn alloc<T: Collect<Id>>(&self, value: T) -> Gc<'_, T, Id> {
        self.alloc_with(|| value)
    }

    #[inline(always)]
    pub fn alloc_with<T: Collect<Id>>(&self, func: impl FnOnce() -> T) -> Gc<'_, T, Id> {
        let allocatd = self.allocator.alloc_layout();
        self.gc
    }
    unsafe fn init_alloc<T: CollectorId>(ptr: NonNull<GcBox<T>>) -> Gc<'_, T, Id> {}
}

pub(crate) struct GcTypeInfo<Id> {
    layout_info: GcLayout,
    drop_func: Option<unsafe fn(*mut ())>,
}
trait TypeIdInit<Id: CollectorId, T: Collect<Id>> {
    const TYPE_INFO_VAL: GcTypeInfo<Id> = {
        let layout = GcHeader::determine_layout(Layout::new::<T>());
        let drop_func = if std::mem::needs_drop::<T>() {
            unsafe {
                Some(std::mem::transmute::<_, unsafe fn(*mut ())>(
                    std::ptr::drop_in_place as unsafe fn(*mut T),
                ))
            }
        } else {
            None
        };
        GcTypeInfo {
            layout_info: layout,
            drop_func,
        }
    };
    const TYPE_INFO_REF: &'static GcTypeInfo<Id> = &Self::TYPE_INFO_VAL;
}

#[bitfield(u32)]
struct GcStateBits {
    #[bit(0)]
    forwarded: bool,
    #[bit(1)]
    yong_generation: bool,
}
union HeaderMetadata<Id: CollectorId> {
    type_info: &'static GcTypeInfo<Id>,
    forward_ptr: NonNull<GcHeader<Id>>,
}
union AllocInfo<T> {
    /// The number of bytes before the previous object header.
    ///
    /// This is used to iterate over objects in the young generation.
    ///
    /// If there is no previous object in this chunk, this is zero.
    ///
    /// This is guaranteed to be equal to `self.type_info().layout.overall_layout.size()`
    /// of the previous object.
    prev_object_offset: u32,
    /// The index of the object within the vector of live objects.
    ///
    /// This is used in the old generation.
    live_object_index: u32,
}
#[repr(C)]
pub(crate) struct GcHeader<Id: CollectorId> {
    state_bits: GcStateBits,
    alloc_info: AllocInfo<T>,
    metadata: HeaderMetadata<Id>,
    collector_id: Id,
}
impl<Id: CollectorId> GcHeader<Id> {
    const HEADER_LAYOUT: Layout = Layout::new::<Self>();

    #[inline]
    pub fn id(&self) -> Id {
        self.collector_id
    }

    #[inline]
    unsafe fn resolve_type_info(&self) -> &'static GcTypeInfo<Id> {
        if self.state_bits.forwarded() {
            let forward_ptr = self.metadata.forward_ptr;
            let forward_header = forward_ptr.as_ref();
            debug_assert!(!forward_header.state_bits.forwarded());
            forward_header.metadata.type_info
        } else {
            self.metadata.type_info
        }
    }

    #[inline]
    pub const fn determine_layout(type_layout: Layout) -> GcLayout {
        let Ok((overall_layout, value_offset)) = LayoutExt(Self::HEADER_LAYOUT).extend(type_layout)
        else {
            panic!("Layout error")
        };
        let overall_layout = LayoutExt(overall_layout).pad_to_align();
        GcLayout {
            value_offset,
            overall_layout,
            type_layout,
        }
    }
}

/// TODO: We should cap the alignment of gc-allocated types
///
/// This would be a win by avoiding runtime size calculations
/// and making the `value_offset` a compile-time constant
#[derive(Eq, PartialEq, Debug)]
pub(crate) struct GcLayout {
    pub type_layout: Layout,
    pub overall_layout: Layout,
    /// The offset between the end of the header and the beginning of the value
    pub value_offset: usize,
}

pub struct CollectContext<'newgc, Id: CollectorId> {
    id: Id,
}
impl<'newgc, Id: CollectorId> CollectContext<'newgc, Id> {
    #[inline]
    pub fn id(&self) -> Id {
        self.id
    }
    #[inline]
    unsafe fn trace_gc_ptr(&mut self, target: NonNull<Gc<'_, T, Id>>) {
        let target = target.as_ptr();
        target.cast().write(self.collect_gc_ptr(target.read()));
    }
    unsafe fn collect_gc_ptr<'gc, T: Collect<Id>>(
        &mut self,
        target: Gc<'gc, T, Id>,
    ) -> Gc<'newgc, T::Collected<'newgc>, Id> {
        debug_assert_eq!(target.id(), self.id());
        let header = target.header();

        if header.state_bits.forwarded() {
            header.metadata.forward_ptr
        }
        crate::utils::transmute_arbitrary(target)
    }
}
