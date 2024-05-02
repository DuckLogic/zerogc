use std::alloc::Layout;
use std::cell::Cell;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::ptr::NonNull;

use bitbybit::{bitenum, bitfield};

use crate::context::old::OldGenerationSpace;
use crate::context::young::YoungGenerationSpace;
use crate::gcptr::Gc;
use crate::utils::LayoutExt;
use crate::Collect;

mod old;
mod young;

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

/// An opaque identifier for a specific garbage collector.
///
/// There is not necessarily a single global garbage collector.
/// There can be multiple ones as long as they have separate [`CollectorId`]s.
///
/// ## Safety
/// This type must be `#[repr(C)`] and its alignment must be at most eight bytes.
pub unsafe trait CollectorId: Copy + Debug + Eq + 'static {
    const SINGLETON: Option<SingletonStatus>;
    unsafe fn resolve_collector(&self) -> *mut GarbageCollector<Self>;

    unsafe fn summon_singleton() -> Option<Self>;
}

pub struct GarbageCollector<Id: CollectorId> {
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
        todo!()
    }
}

/// The layout of a "regular" (non-array) type
pub(crate) struct GcTypeLayout<Id: CollectorId> {
    /// The layout of the underlying value
    ///
    /// INVARIANT: The maximum alignment is [`GcHeader::FIXED_ALIGNMENT`]
    value_layout: Layout,
    /// The overall size of the value including the header
    /// and trailing padding.
    overall_size: usize,
    marker: PhantomData<&'static Id>,
}
impl<Id: CollectorId> GcTypeLayout<Id> {
    #[inline]
    pub const fn value_size(&self) -> usize {
        self.value_layout.size()
    }

    #[inline]
    pub const fn value_align(&self) -> usize {
        self.value_layout.align()
    }

    #[inline]
    pub const fn value_layout(&self) -> Layout {
        self.value_layout
    }

    #[inline]
    pub const fn overall_layout(&self) -> Layout {
        unsafe {
            Layout::from_size_align_unchecked(self.overall_size, GcHeader::<Id>::FIXED_ALIGNMENT)
        }
    }

    //noinspection RsAssertEqual
    const fn compute_overall_layout(value_layout: Layout) -> Layout {
        let header_layout = GcHeader::<Id>::REGULAR_HEADER_LAYOUT;
        let Ok((expected_overall_layout, value_offset)) =
            LayoutExt(header_layout).extend(value_layout)
        else {
            panic!("layout overflow")
        };
        assert!(
            value_offset == GcHeader::<Id>::REGULAR_VALUE_OFFSET,
            "Unexpected value offset"
        );
        let res = LayoutExt(expected_overall_layout).pad_to_align();
        assert!(
            res.align() == GcHeader::<Id>::FIXED_ALIGNMENT,
            "Unexpected overall alignment"
        );
        res
    }
    #[track_caller]
    pub const fn from_value_layout(value_layout: Layout) -> Self {
        assert!(
            value_layout.align() <= GcHeader::<Id>::FIXED_ALIGNMENT,
            "Alignment exceeds maximum",
        );
        let overall_layout = Self::compute_overall_layout(value_layout);
        GcTypeLayout {
            value_layout,
            overall_size: overall_layout.size(),
            marker: PhantomData,
        }
    }
}

#[repr(transparent)]
pub(crate) struct GcArrayTypeInfo<Id: CollectorId> {
    /// The type info for the array's elements.
    ///
    /// This is stored as the first element to allow one-way
    /// pointer casts from GcArrayTypeInfo -> GcTypeInfo.
    /// This simulates OO-style inheritance.
    element_type_info: GcTypeInfo<Id>,
}

impl<Id: CollectorId> GcArrayTypeInfo<Id> {
    //noinspection RsAssertEqual
    #[inline]
    pub const fn new<T: Collect<Id>>() -> &'static Self {
        /*
         * for the time being GcTypeInfo <--> GcArrayTypeInfo,
         * so we just cast the pointers
         */
        assert!(std::mem::size_of::<Self>() == std::mem::size_of::<GcTypeInfo<Id>>());
        unsafe {
            &*(GcTypeInfo::<Id>::new::<T>() as *const GcTypeInfo<Id> as *const GcArrayTypeInfo<Id>)
        }
    }
}

pub(crate) struct GcTypeInfo<Id: CollectorId> {
    layout: GcTypeLayout<Id>,
    drop_func: Option<unsafe fn(*mut ())>,
}
impl<Id: CollectorId> GcTypeInfo<Id> {
    #[inline]
    pub const fn new<T: Collect<Id>>() -> &'static Self {
        <GcTypeInitImpl as TypeIdInit<Id, T>>::TYPE_INFO_REF
    }
}
trait TypeIdInit<Id: CollectorId, T: Collect<Id>> {
    const TYPE_INFO_INIT_VAL: GcTypeInfo<Id> = {
        let layout = GcTypeLayout::from_value_layout(Layout::new::<T>());
        let drop_func = if std::mem::needs_drop::<T>() {
            unsafe {
                Some(std::mem::transmute::<_, unsafe fn(*mut ())>(
                    std::ptr::drop_in_place as unsafe fn(*mut T),
                ))
            }
        } else {
            None
        };
        GcTypeInfo { layout, drop_func }
    };
    const TYPE_INFO_REF: &'static GcTypeInfo<Id> = &Self::TYPE_INFO_INIT_VAL;
}
struct GcTypeInitImpl;
impl<Id: CollectorId, T: Collect<Id>> TypeIdInit<Id, T> for GcTypeInitImpl {}

unsafe trait Generation<Id: CollectorId> {
    const ID: GenerationId;
    fn cast_young(&self) -> Option<&'_ YoungGenerationSpace<Id>>;
    fn cast_old(&self) -> Option<&'_ OldGenerationSpace<Id>>;
}

#[derive(Debug, Eq, PartialEq)]
#[bitenum(u1, exhaustive = true)]
enum GenerationId {
    Young = 0,
    Old = 1,
}

/// The raw bit representation of [GcMarkBits]
type GcMarkBitsRepr = arbitrary_int::UInt<u8, 1>;

#[bitenum(u1, exhaustive = true)]
enum GcMarkBits {
    White = 0,
    Black = 1,
}

impl GcMarkBits {
    #[inline]
    pub fn to_raw<Id: CollectorId, T: Generation<Id>>(&self, gen: &T) -> GcRawMarkBits {
        let bits: GcMarkBitsRepr = self.raw_value();
        GcRawMarkBits::new_with_raw_value(if GcRawMarkBits::is_inverted(gen) {
            GcRawMarkBits::invert_bits(bits)
        } else {
            bits
        })
    }
}

#[bitenum(u1, exhaustive = true)]
enum GcRawMarkBits {
    Red = 0,
    Green = 1,
}
impl GcRawMarkBits {
    #[inline]
    pub fn resolve<Id: CollectorId, T: Generation<Id>>(&self, gen: &T) -> GcMarkBits {
        let bits: GcMarkBitsRepr = self.raw_value();
        GcMarkBits::new_with_raw_value(if Self::is_inverted(gen) {
            Self::invert_bits(bits)
        } else {
            bits
        })
    }

    #[inline]
    fn invert_bits(bits: GcMarkBitsRepr) -> GcMarkBitsRepr {
        <GcMarkBitsRepr as arbitrary_int::Number>::MAX - bits
    }

    #[inline]
    fn is_inverted<Id: CollectorId, T: Generation<Id>>(gen: &T) -> bool {
        match T::Id {
            GenerationId::Young => false,
            GenerationId::Old => gen.cast_old().unwrap().mark_bits_inverted(),
        }
    }
}

/// A bitfield for the garbage collector's state.
///
/// ## Default
/// The `DEFAULT` value isn't valid here.
/// However, it currently needs to exist fo
/// the macro to generate the `builder` field
#[bitfield(u32, default = 0)]
struct GcStateBits {
    #[bit(0, rw)]
    forwarded: bool,
    #[bit(1, rw)]
    generation: GenerationId,
    #[bit(2, rw)]
    array: bool,
    #[bit(3, rw)]
    raw_mark_bits: GcRawMarkBits,
}
union HeaderMetadata<Id: CollectorId> {
    type_info: &'static GcTypeInfo<Id>,
    array_type_info: &'static GcArrayTypeInfo<Id>,
    forward_ptr: NonNull<GcHeader<Id>>,
}
union AllocInfo {
    /// The [overall size][`GcTypeLayout::overall_layout`] of this object.
    ///
    /// This is used to iterate over objects in the young generation.
    ///
    /// Objects whose size cannot fit into a `u32`
    /// can never be allocated in the young generation.
    ///
    /// If this object is an array,
    /// this is the overall size of
    /// the header and all elements.
    pub this_object_overall_size: u32,
    /// The index of the object within the vector of live objects.
    ///
    /// This is used in the old generation.
    pub live_object_index: u32,
}

#[repr(C, align(8))]
pub(crate) struct GcHeader<Id: CollectorId> {
    pub state_bits: Cell<GcStateBits>,
    pub alloc_info: AllocInfo,
    pub metadata: HeaderMetadata<Id>,
    /// The id for the collector where this object is allocated.
    ///
    /// If the collector is a singleton (either global or thread-local),
    /// this will be a zero sized type.
    ///
    /// ## Safety
    /// The alignment of this type must be smaller than [`GcHeader::FIXED_ALIGNMENT`].
    pub collector_id: Id,
}
impl<Id: CollectorId> GcHeader<Id> {
    /// The fixed alignment for all GC types
    ///
    /// Allocating a type with an alignment greater than this is an error.
    pub const FIXED_ALIGNMENT: usize = 8;
    /// The fixed offset from the start of the GcHeader to a regular value
    pub const REGULAR_VALUE_OFFSET: usize = std::mem::size_of::<Self>();
    pub const ARRAY_VALUE_OFFSET: usize = std::mem::size_of::<GcArrayHeader<Id>>();
    const REGULAR_HEADER_LAYOUT: Layout = Layout::new::<Self>();
    const ARRAY_HEADER_LAYOUT: Layout = Layout::new::<GcArrayHeader<Id>>();

    #[inline]
    pub fn id(&self) -> Id {
        self.collector_id
    }

    #[inline]
    fn resolve_type_info(&self) -> &'static GcTypeInfo<Id> {
        unsafe {
            if self.state_bits.get().forwarded() {
                let forward_ptr = self.metadata.forward_ptr;
                let forward_header = forward_ptr.as_ref();
                debug_assert!(!forward_header.state_bits.get().forwarded());
                forward_header.metadata.type_info
            } else {
                self.metadata.type_info
            }
        }
    }

    #[inline]
    pub fn regular_value_ptr(&self) -> NonNull<u8> {
        unsafe {
            NonNull::new_unchecked(
                (self as *const Self as *mut Self as *mut u8).add(Self::REGULAR_VALUE_OFFSET),
            )
        }
    }

    #[inline]
    pub unsafe fn assume_array_header(&self) -> &'_ GcArrayHeader<Id> {
        &*(self as *const Self as *const GcArrayHeader<Id>)
    }
}

#[repr(C, align(8))]
pub struct GcArrayHeader<Id: CollectorId> {
    main_header: GcHeader<Id>,
    /// The length of the array in elements
    len_elements: usize,
}

impl<Id: CollectorId> GcArrayHeader<Id> {
    #[inline]
    fn resolve_type_info(&self) -> &'static GcArrayTypeInfo<Id> {
        unsafe {
            &*(self.main_header.resolve_type_info() as *const GcTypeInfo<Id>
                as *const GcArrayTypeInfo<Id>)
        }
    }

    #[inline]
    fn element_layout(&self) -> Layout {
        self.resolve_type_info()
            .element_type_info
            .layout
            .value_layout
    }

    #[cfg_attr(not(debug_assertions), inline)]
    fn value_layout(&self) -> Layout {
        let element_layout = self.element_layout();
        if cfg!(debug_assertions) {
            debug_assert!(element_layout.align() <= GcHeader::<Id>::FIXED_ALIGNMENT);
            debug_assert_eq!(
                element_layout.pad_to_align(),
                element_layout,
                "padding should already be included"
            );
            let Some(repeated_size) = element_layout.size().checked_mul(self.len_elements) else {
                panic!(
                    "Invalid length {} triggers size overflow for {element_layout:?}",
                    self.len_elements
                )
            };
            debug_assert!(
                Layout::from_size_align(repeated_size, element_layout.align()).is_ok(),
                "align overflow"
            );
        }
        unsafe {
            Layout::from_size_align_unchecked(
                element_layout.size().unchecked_mul(self.len_elements),
                element_layout.align(),
            )
        }
    }

    #[cfg_attr(not(debug_assertions), inline)]
    fn overall_layout(&self) -> Layout {
        let value_layout = self.value_layout();
        if cfg!(debug_assertions) {
            let Ok((overall_layout, actual_offset)) =
                LayoutExt(GcHeader::<Id>::ARRAY_HEADER_LAYOUT).extend(value_layout)
            else {
                unreachable!("layout overflow")
            };
            debug_assert_eq!(actual_offset, GcHeader::<Id>::ARRAY_VALUE_OFFSET);
            debug_assert_eq!(
                Some(overall_layout.size()),
                value_layout
                    .size()
                    .checked_add(GcHeader::<Id>::ARRAY_VALUE_OFFSET)
            );
        }
        unsafe {
            Layout::from_size_align_unchecked(
                value_layout
                    .size()
                    .unchecked_add(GcHeader::<Id>::ARRAY_VALUE_OFFSET),
                GcHeader::<Id>::FIXED_ALIGNMENT,
            )
            .pad_to_align()
        }
    }
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
    pub unsafe fn trace_gc_ptr_mut<T: Collect<Id>>(&mut self, target: NonNull<Gc<'_, T, Id>>) {
        let target = target.as_ptr();
        target
            .cast::<Gc<'newgc, T::Collected<'newgc>, Id>>()
            .write(self.collect_gc_ptr(target.read()));
    }

    unsafe fn collect_gc_ptr<'gc, T: Collect<Id>>(
        &mut self,
        target: Gc<'gc, T, Id>,
    ) -> Gc<'newgc, T::Collected<'newgc>, Id> {
        debug_assert_eq!(target.id(), self.id());
        let header = target.header();
        if header.state_bits.get().forwarded() {
            return Gc::from_raw_ptr(
                header
                    .metadata
                    .forward_ptr
                    .as_ref()
                    .regular_value_ptr()
                    .cast(),
            );
        }
        todo!()
    }
}
