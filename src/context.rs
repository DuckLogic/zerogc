use std::alloc::Layout;
use std::cell::Cell;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::ptr::NonNull;

use bitbybit::bitenum;

use crate::context::layout::{
    GcArrayHeader, GcArrayLayoutInfo, GcArrayTypeInfo, GcHeader, GcMarkBits, GcStateBits,
    GcTypeInfo, HeaderMetadata, TraceFuncPtr,
};
use crate::context::old::OldGenerationSpace;
use crate::context::young::YoungGenerationSpace;
use crate::gcptr::Gc;
use crate::Collect;

pub(crate) mod layout;
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

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum CollectStageTracker {
    NotCollecting,
    Stage { current: CollectStage },
    FinishedStage { last_stage: CollectStage },
}

impl CollectStageTracker {
    #[inline]
    fn begin_stage(&mut self, expected_stage: Option<CollectStage>, new_stage: CollectStage) {
        assert_eq!(
            match expected_stage {
                Some(last_stage) => CollectStageTracker::FinishedStage { last_stage },
                None => CollectStageTracker::NotCollecting,
            },
            *self
        );
        *self = CollectStageTracker::Stage { current: new_stage };
    }

    #[inline]
    fn finish_stage(&mut self, stage: CollectStage) {
        assert_eq!(CollectStageTracker::Stage { current: stage }, *self);
        *self = CollectStageTracker::FinishedStage { last_stage: stage };
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum CollectStage {
    Mark,
    Sweep,
}

/// The state of a [GarbageCollector]
///
/// Seperated out to pass around as a separate reference.
/// This is important to avoid `&mut` from different sub-structures.
pub(crate) struct CollectorState<Id: CollectorId> {
    collector_id: Id,
    mark_bits_inverted: Cell<bool>,
}

pub struct GarbageCollector<Id: CollectorId> {
    state: CollectorState<Id>,
    young_generation: YoungGenerationSpace<Id>,
    old_generation: OldGenerationSpace<Id>,
}
impl<Id: CollectorId> GarbageCollector<Id> {
    #[inline]
    pub fn id(&self) -> Id {
        self.state.collector_id
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

unsafe trait RawAllocTarget<Id: CollectorId> {
    const ARRAY: bool;
    type Header: Sized;
    fn header_metadata(&self) -> HeaderMetadata<Id>;
    unsafe fn init_header(&self, header_ptr: NonNull<Self::Header>, base_header: GcHeader<Id>);
    fn overall_layout(&self) -> Layout;
    #[inline]
    fn init_state_bits(&self, gen: GenerationId) -> GcStateBits {
        GcStateBits::builder()
            .with_forwarded(false)
            .with_generation(gen)
            .with_array(Self::ARRAY)
            .with_raw_mark_bits(GcMarkBits::White.to_raw(self.collector_state()))
            .build()
    }

    fn collector_state(&self) -> &'_ CollectorState<Id>;
}
struct RegularAlloc<'a, Id: CollectorId> {
    state: &'a CollectorState<Id>,
    type_info: &'static GcTypeInfo<Id>,
}
unsafe impl<Id: CollectorId> RawAllocTarget<Id> for RegularAlloc<'_, Id> {
    const ARRAY: bool = false;
    type Header = GcHeader<Id>;

    #[inline]
    fn header_metadata(&self) -> HeaderMetadata<Id> {
        HeaderMetadata {
            type_info: self.type_info,
        }
    }

    #[inline]
    unsafe fn init_header(&self, header_ptr: NonNull<GcHeader<Id>>, base_header: GcHeader<Id>) {
        header_ptr.as_ptr().write(base_header)
    }

    #[inline]
    fn overall_layout(&self) -> Layout {
        unsafe {
            Layout::from_size_align_unchecked(
                self.type_info.layout.overall_layout().size(),
                GcHeader::<Id>::FIXED_ALIGNMENT,
            )
        }
    }

    #[inline]
    fn collector_state(&self) -> &'_ CollectorState<Id> {
        self.state
    }
}
struct ArrayAlloc<'a, Id: CollectorId> {
    type_info: &'static GcArrayTypeInfo<Id>,
    layout_info: GcArrayLayoutInfo<Id>,
    state: &'a CollectorState<Id>,
}
unsafe impl<Id: CollectorId> RawAllocTarget<Id> for ArrayAlloc<'_, Id> {
    const ARRAY: bool = true;
    type Header = GcArrayHeader<Id>;

    #[inline]
    fn header_metadata(&self) -> HeaderMetadata<Id> {
        HeaderMetadata {
            array_type_info: self.type_info,
        }
    }

    #[inline]
    unsafe fn init_header(
        &self,
        header_ptr: NonNull<GcArrayHeader<Id>>,
        base_header: GcHeader<Id>,
    ) {
        header_ptr.as_ptr().write(GcArrayHeader {
            main_header: base_header,
            len_elements: self.layout_info.len_elements(),
        })
    }

    #[inline]
    fn overall_layout(&self) -> Layout {
        self.layout_info.overall_layout()
    }

    #[inline]
    fn collector_state(&self) -> &'_ CollectorState<Id> {
        self.state
    }
}

#[derive(Debug, Eq, PartialEq)]
#[bitenum(u1, exhaustive = true)]
enum GenerationId {
    Young = 0,
    Old = 1,
}

pub struct CollectContext<'newgc, Id: CollectorId> {
    id: Id,
    garbage_collector: &'newgc mut GarbageCollector<Id>,
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

    #[cfg_attr(not(debug_assertions), inline)]
    unsafe fn collect_gc_ptr<'gc, T: Collect<Id>>(
        &mut self,
        target: Gc<'gc, T, Id>,
    ) -> Gc<'newgc, T::Collected<'newgc>, Id> {
        let header = target.header();
        assert_eq!(header.collector_id, self.id, "Mismatched collector ids");
        debug_assert!(
            !header.state_bits.get().array(),
            "Incorrectly marked as an array"
        );
        if header.state_bits.get().forwarded() {
            debug_assert_eq!(header.state_bits.get().generation(), GenerationId::Young);
            debug_assert_eq!(
                header
                    .state_bits
                    .get()
                    .raw_mark_bits()
                    .resolve(&self.garbage_collector.state),
                GcMarkBits::Black
            );
            return Gc::from_raw_ptr(
                header
                    .metadata
                    .forward_ptr
                    .as_ref()
                    .regular_value_ptr()
                    .cast(),
            );
        }
        match header
            .state_bits
            .get()
            .raw_mark_bits()
            .resolve(&self.garbage_collector.state)
        {
            GcMarkBits::White => {
                let new_header = self.fallback_collect_gc_header(NonNull::from(header));
                Gc::from_raw_ptr(new_header.as_ref().regular_value_ptr().cast())
            }
            GcMarkBits::Black => {
                // already traced, can skip it
                Gc::from_raw_ptr(target.as_raw_ptr().cast())
            }
        }
    }

    #[cold]
    unsafe fn fallback_collect_gc_header<'gc>(
        &mut self,
        header_ptr: NonNull<GcHeader<Id>>,
    ) -> NonNull<GcHeader<Id>> {
        let type_info: &'static GcTypeInfo<Id>;
        let array = header_ptr.as_ref().state_bits.get().array();
        let prev_generation: GenerationId;
        {
            let header = header_ptr.as_ref();
            debug_assert_eq!(
                header
                    .state_bits
                    .get()
                    .raw_mark_bits()
                    .resolve(&self.garbage_collector.state),
                GcMarkBits::White
            );
            // mark as black
            header.update_state_bits(|state_bits| {
                state_bits
                    .with_raw_mark_bits(GcMarkBits::Black.to_raw(&self.garbage_collector.state));
            });
            prev_generation = header.state_bits.get().generation();
            type_info = header.metadata.type_info;
        }
        let forwarded_ptr = match prev_generation {
            GenerationId::Young => {
                let array_value_size: Option<usize>;
                // reallocate in oldgen
                let copied_ptr = if array {
                    let array_type_info = type_info.assume_array_info();
                    debug_assert!(std::ptr::eq(
                        array_type_info,
                        header_ptr.as_ref().metadata.array_type_info
                    ));
                    let array_layout = GcArrayLayoutInfo::new_unchecked(
                        array_type_info.element_type_info.layout.value_layout(),
                        header_ptr.cast::<GcArrayHeader<Id>>().as_ref().len_elements,
                    );
                    array_value_size = Some(array_layout.value_layout().size());
                    self.garbage_collector
                        .old_generation
                        .alloc_raw(ArrayAlloc {
                            layout_info: array_layout,
                            type_info: array_type_info,
                            state: &self.garbage_collector.state,
                        })
                        .map(NonNull::cast::<GcHeader<Id>>)
                } else {
                    array_value_size = None;
                    self.garbage_collector
                        .old_generation
                        .alloc_raw(RegularAlloc {
                            type_info,
                            state: &self.garbage_collector.state,
                        })
                }
                .unwrap_or_else(|_| {
                    // TODO: This panic is fatal, will cause an abort
                    panic!("Oldgen alloc failure")
                });
                copied_ptr
                    .as_ref()
                    .state_bits
                    .set(header_ptr.as_ref().state_bits.get());
                copied_ptr.as_ref().update_state_bits(|bits| {
                    debug_assert!(!bits.forwarded());
                    bits.with_generation(GenerationId::Old);
                });
                header_ptr.as_ref().update_state_bits(|bits| {
                    bits.with_forwarded(true);
                });
                (&mut *header_ptr.as_ptr()).metadata.forward_ptr = copied_ptr.cast();
                // NOTE: Copy uninitialized bytes is safe here, as long as they are not read in dest
                if array {
                    copied_ptr
                        .cast::<GcArrayHeader<Id>>()
                        .as_ref()
                        .array_value_ptr()
                        .cast::<u8>()
                        .as_ptr()
                        .copy_from_nonoverlapping(
                            header_ptr
                                .cast::<GcArrayHeader<Id>>()
                                .as_ref()
                                .array_value_ptr()
                                .as_ptr(),
                            array_value_size.unwrap(),
                        )
                } else {
                    copied_ptr
                        .as_ref()
                        .regular_value_ptr()
                        .cast::<u8>()
                        .as_ptr()
                        .copy_from_nonoverlapping(
                            header_ptr
                                .as_ref()
                                .regular_value_ptr()
                                .cast::<u8>()
                                .as_ptr(),
                            type_info.layout.value_layout().size(),
                        );
                }
                copied_ptr
            }
            GenerationId::Old => header_ptr, // no copying needed for oldgen
        };
        /*
         * finally, trace the value
         * this needs to come after forwarding and switching the mark bit
         * so we can properly update self-referential pointers
         */
        if let Some(trace_func) = type_info.trace_func {
            /*
             * NOTE: Cannot have aliasing &mut header references during this recursion
             * The parameters to maybe_grow are completely arbitrary right now.
             */
            stacker::maybe_grow(
                4096,       // 4KB
                128 * 1024, // 128KB
                || self.trace_children(forwarded_ptr, trace_func),
            );
        }
        forwarded_ptr
    }

    #[inline]
    unsafe fn trace_children(
        &mut self,
        header: NonNull<GcHeader<Id>>,
        trace_func: TraceFuncPtr<Id>,
    ) {
        debug_assert!(
            !header.as_ref().state_bits.get().forwarded(),
            "Cannot be forwarded"
        );
        if header.as_ref().state_bits.get().array() {
            self.trace_children_array(header.cast(), trace_func);
        } else {
            trace_func(header.as_ref().regular_value_ptr().cast(), self);
        }
    }
    unsafe fn trace_children_array(
        &mut self,
        header: NonNull<GcArrayHeader<Id>>,
        trace_func: TraceFuncPtr<Id>,
    ) {
        let type_info = header.as_ref().main_header.metadata.type_info;
        debug_assert_eq!(type_info.trace_func, Some(trace_func));
        let array_header = header.cast::<GcArrayHeader<Id>>();
        let element_layout = type_info.layout.value_layout();
        let len = array_header.as_ref().len_elements;
        let element_start_ptr = array_header.as_ref().array_value_ptr();
        for i in 0..len {
            let element = element_start_ptr
                .as_ptr()
                .add(i.unchecked_mul(element_layout.size()));
            trace_func(NonNull::new_unchecked(element as *mut ()), self);
        }
    }
}
