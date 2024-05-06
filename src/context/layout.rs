use crate::context::{CollectorState, GenerationId};
use crate::utils::LayoutExt;
use crate::{Collect, CollectContext, CollectorId};
use bitbybit::{bitenum, bitfield};
use std::alloc::Layout;
use std::cell::Cell;
use std::fmt::{Debug, Formatter};
use std::iter::FusedIterator;
use std::marker::PhantomData;
use std::path::Iter;
use std::ptr::NonNull;
use std::thread::current;

/// The layout of a "regular" (non-array) type
#[derive(Debug)]
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
    pub(super) element_type_info: GcTypeInfo<Id>,
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

pub type TraceFuncPtr<Id> = unsafe fn(NonNull<()>, &mut CollectContext<Id>);

#[repr(C)]
#[derive(Debug)]
pub(crate) struct GcTypeInfo<Id: CollectorId> {
    pub(super) layout: GcTypeLayout<Id>,
    pub(super) drop_func: Option<unsafe fn(*mut ())>,
    pub(super) trace_func: Option<TraceFuncPtr<Id>>,
}
impl<Id: CollectorId> GcTypeInfo<Id> {
    #[inline]
    pub unsafe fn assume_array_info(&self) -> &'_ GcArrayTypeInfo<Id> {
        // Takes advantage of fact repr is identical
        assert_eq!(
            std::mem::size_of::<Self>(),
            std::mem::size_of::<GcArrayTypeInfo<Id>>()
        );
        &*(self as *const Self as *const GcArrayTypeInfo<Id>)
    }

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
        let trace_func = if T::NEEDS_COLLECT {
            unsafe {
                Some(std::mem::transmute::<
                    _,
                    unsafe fn(NonNull<()>, &mut CollectContext<Id>),
                >(
                    T::collect_inplace as unsafe fn(NonNull<T>, &mut CollectContext<Id>),
                ))
            }
        } else {
            None
        };
        GcTypeInfo {
            layout,
            drop_func,
            trace_func,
        }
    };
    const TYPE_INFO_REF: &'static GcTypeInfo<Id> = &Self::TYPE_INFO_INIT_VAL;
}
struct GcTypeInitImpl;
impl<Id: CollectorId, T: Collect<Id>> TypeIdInit<Id, T> for GcTypeInitImpl {}

/// The raw bit representation of [crate::context::GcMarkBits]
type GcMarkBitsRepr = arbitrary_int::UInt<u8, 1>;

#[derive(Debug, Eq, PartialEq)]
#[bitenum(u1, exhaustive = true)]
pub enum GcMarkBits {
    /// Indicates that tracing has not yet marked the object.
    ///
    /// Once tracing completes, this means the object is dead.
    White = 0,
    /// Indicates that tracing has marked the object.
    ///
    /// This means the object is live.
    Black = 1,
}

impl GcMarkBits {
    #[inline]
    pub fn to_raw<Id: CollectorId>(self, state: &CollectorState<Id>) -> GcRawMarkBits {
        let bits: GcMarkBitsRepr = self.raw_value();
        GcRawMarkBits::new_with_raw_value(if state.mark_bits_inverted.get() {
            GcRawMarkBits::invert_bits(bits)
        } else {
            bits
        })
    }
}

#[bitenum(u1, exhaustive = true)]
pub enum GcRawMarkBits {
    Red = 0,
    Green = 1,
}
impl GcRawMarkBits {
    #[inline]
    pub fn resolve<Id: CollectorId>(&self, state: &CollectorState<Id>) -> GcMarkBits {
        let bits: GcMarkBitsRepr = self.raw_value();
        GcMarkBits::new_with_raw_value(if state.mark_bits_inverted.get() {
            Self::invert_bits(bits)
        } else {
            bits
        })
    }

    #[inline]
    fn invert_bits(bits: GcMarkBitsRepr) -> GcMarkBitsRepr {
        <GcMarkBitsRepr as arbitrary_int::Number>::MAX - bits
    }
}

/// A bitfield for the garbage collector's state.
///
/// ## Default
/// The `DEFAULT` value isn't valid here.
/// However, it currently needs to exist fo
/// the macro to generate the `builder` field
#[bitfield(u32, default = 0)]
#[derive(Debug)]
pub struct GcStateBits {
    #[bit(0, rw)]
    forwarded: bool,
    #[bit(1, rw)]
    generation: GenerationId,
    #[bit(2, rw)]
    array: bool,
    #[bit(3, rw)]
    raw_mark_bits: GcRawMarkBits,
    #[bit(4, rw)]
    value_initialized: bool,
}
pub union HeaderMetadata<Id: CollectorId> {
    pub type_info: &'static GcTypeInfo<Id>,
    pub array_type_info: &'static GcArrayTypeInfo<Id>,
    pub forward_ptr: NonNull<GcHeader<Id>>,
}
pub union AllocInfo {
    /// The index of this object within the vector of objects which need to be dropped.
    ///
    /// If this object doesn't need to be dropped,
    /// then this is `u32::MAX`
    ///
    /// This is used in the young generation.
    pub nontrivial_drop_index: u32,
    /// The index of the object within the vector of live objects.
    ///
    /// This is used in the old generation.
    pub live_object_index: u32,
}

#[repr(C, align(8))]
pub(crate) struct GcHeader<Id: CollectorId> {
    pub(super) state_bits: Cell<GcStateBits>,
    pub(super) alloc_info: AllocInfo,
    pub(super) metadata: HeaderMetadata<Id>,
    /// The id for the collector where this object is allocated.
    ///
    /// If the collector is a singleton (either global or thread-local),
    /// this will be a zero sized type.
    ///
    /// ## Safety
    /// The alignment of this type must be smaller than [`GcHeader::FIXED_ALIGNMENT`].
    pub collector_id: Id,
}
impl<Id: CollectorId> Debug for GcHeader<Id> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcHeader")
            .field("state_bits", &self.state_bits.get())
            .field(
                "metadata",
                if self.state_bits.get().forwarded() {
                    unsafe { &self.metadata.forward_ptr }
                } else {
                    unsafe { &self.metadata.type_info }
                },
            )
            .field("collector_id", &self.collector_id)
            .finish_non_exhaustive()
    }
}
impl<Id: CollectorId> GcHeader<Id> {
    #[inline]
    pub(crate) unsafe fn update_state_bits(&self, func: impl FnOnce(GcStateBits) -> GcStateBits) {
        self.state_bits.set(func(self.state_bits.get()));
    }

    /// The fixed alignment for all GC types
    ///
    /// Allocating a type with an alignment greater than this is an error.
    pub const FIXED_ALIGNMENT: usize = 8;
    /// The fixed offset from the start of the GcHeader to a regular value
    pub const REGULAR_VALUE_OFFSET: usize = std::mem::size_of::<Self>();
    pub const ARRAY_VALUE_OFFSET: usize = std::mem::size_of::<GcArrayHeader<Id>>();
    pub const REGULAR_HEADER_LAYOUT: Layout = Layout::new::<Self>();
    pub const ARRAY_HEADER_LAYOUT: Layout = Layout::new::<GcArrayHeader<Id>>();

    #[inline]
    pub fn id(&self) -> Id {
        self.collector_id
    }

    #[inline]
    pub fn resolve_type_info(&self) -> &'static GcTypeInfo<Id> {
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

    #[inline]
    pub unsafe fn invoke_destructor(&self) {
        if let Some(drop_func) = self.resolve_type_info().drop_func {
            drop_func(self.regular_value_ptr().as_ptr() as *mut ());
        }
    }
}

#[repr(C, align(8))]
pub struct GcArrayHeader<Id: CollectorId> {
    pub(super) main_header: GcHeader<Id>,
    /// The length of the array in elements
    pub(super) len_elements: usize,
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
    pub fn array_value_ptr(&self) -> NonNull<u8> {
        unsafe {
            NonNull::new_unchecked(
                (self as *const Self as *mut Self as *mut u8)
                    .add(GcHeader::<Id>::ARRAY_VALUE_OFFSET),
            )
        }
    }

    #[inline]
    pub fn layout_info(&self) -> GcArrayLayoutInfo<Id> {
        GcArrayLayoutInfo {
            element_layout: self.element_layout(),
            len_elements: self.len_elements,
            marker: PhantomData,
        }
    }

    #[inline]
    fn element_layout(&self) -> Layout {
        self.resolve_type_info()
            .element_type_info
            .layout
            .value_layout
    }

    #[inline]
    fn value_layout(&self) -> Layout {
        self.layout_info().value_layout()
    }

    #[inline]
    fn overall_layout(&self) -> Layout {
        self.layout_info().overall_layout()
    }

    #[inline]
    pub unsafe fn iter_elements(&self) -> IterArrayElementPtr {
        let len = self.len_elements;
        IterArrayElementPtr {
            element_size: self.element_layout().size(),
            current_ptr: self.array_value_ptr(),
            remaining_elements: len,
        }
    }

    pub unsafe fn invoke_destructor(&self) {
        if let Some(drop_func) = self.resolve_type_info().element_type_info.drop_func {
            for element in self.iter_elements() {
                drop_func(element.as_ptr() as *mut ());
            }
        }
    }
}

pub struct IterArrayElementPtr {
    element_size: usize,
    current_ptr: NonNull<u8>,
    remaining_elements: usize,
}
impl Iterator for IterArrayElementPtr {
    type Item = NonNull<u8>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining_elements > 0 {
            let element_ptr = self.current_ptr;
            unsafe {
                self.current_ptr =
                    NonNull::new_unchecked(element_ptr.as_ptr().add(self.element_size));
            }
            self.remaining_elements -= 1;
            Some(element_ptr)
        } else {
            None
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining_elements, Some(self.remaining_elements))
    }
}
impl ExactSizeIterator for IterArrayElementPtr {}
impl FusedIterator for IterArrayElementPtr {}

pub struct GcArrayLayoutInfo<Id: CollectorId> {
    element_layout: Layout,
    len_elements: usize,
    marker: PhantomData<&'static Id>,
}
impl<Id: CollectorId> GcArrayLayoutInfo<Id> {
    #[inline]
    pub unsafe fn new_unchecked(element_layout: Layout, len_elements: usize) -> Self {
        #[cfg(debug_assertions)]
        {
            match Self::new(element_layout, len_elements) {
                Ok(_success) => {}
                Err(GcArrayLayoutError::ArraySizeOverflow) => {
                    panic!("Invalid array layout: size overflow")
                }
                Err(_) => panic!("invalid array layout: other issue"),
            }
        }
        GcArrayLayoutInfo {
            element_layout,
            len_elements,
            marker: PhantomData,
        }
    }

    // See Layout::max_size_for_align
    const MAX_VALUE_SIZE: usize = ((isize::MAX as usize) - GcHeader::<Id>::FIXED_ALIGNMENT - 1)
        - GcHeader::<Id>::ARRAY_VALUE_OFFSET;

    #[cfg_attr(not(debug_assertions), inline)]
    pub const fn new(
        element_layout: Layout,
        len_elements: usize,
    ) -> Result<Self, GcArrayLayoutError> {
        if element_layout.align() > GcHeader::<Id>::FIXED_ALIGNMENT {
            return Err(GcArrayLayoutError::InvalidElementAlign);
        }
        if LayoutExt(element_layout).pad_to_align().size() != element_layout.size() {
            return Err(GcArrayLayoutError::ElementMissingPadding);
        }
        let Some(repeated_value_size) = element_layout.size().checked_mul(len_elements) else {
            return Err(GcArrayLayoutError::ArraySizeOverflow);
        };
        if repeated_value_size >= Self::MAX_VALUE_SIZE {
            return Err(GcArrayLayoutError::ArraySizeOverflow);
        }
        if cfg!(debug_assertions) {
            // double check above calculations
            match Layout::from_size_align(repeated_value_size, GcHeader::<Id>::FIXED_ALIGNMENT) {
                Ok(repeated_value) => {
                    match LayoutExt(GcHeader::<Id>::ARRAY_HEADER_LAYOUT).extend(repeated_value) {
                        Ok((overall_layout, actual_offset)) => {
                            debug_assert!(actual_offset == GcHeader::<Id>::ARRAY_VALUE_OFFSET);
                            debug_assert!(
                                overall_layout.size()
                                    == match repeated_value_size
                                        .checked_add(GcHeader::<Id>::ARRAY_VALUE_OFFSET)
                                    {
                                        Some(size) => size,
                                        None => panic!("checked add overflow"),
                                    }
                            );
                        }
                        Err(_e) => panic!("Overall value overflows layout"),
                    }
                }
                Err(_) => panic!("Repeated value overflows layout!"),
            }
        }
        Ok(GcArrayLayoutInfo {
            element_layout,
            len_elements,
            marker: PhantomData,
        })
    }

    #[inline]
    pub const fn len_elements(&self) -> usize {
        self.len_elements
    }

    #[inline]
    pub const fn element_layout(&self) -> Layout {
        self.element_layout
    }

    #[inline]
    pub fn value_layout(&self) -> Layout {
        let element_layout = self.element_layout();
        unsafe {
            Layout::from_size_align_unchecked(
                element_layout.size().unchecked_mul(self.len_elements),
                element_layout.align(),
            )
        }
    }

    #[inline]
    pub fn overall_layout(&self) -> Layout {
        let value_layout = self.value_layout();
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
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GcArrayLayoutError {
    #[error("Invalid element alignment")]
    InvalidElementAlign,
    #[error("Element layout missing trailing padding")]
    ElementMissingPadding,
    #[error("Size overflow for array layout")]
    ArraySizeOverflow,
}
