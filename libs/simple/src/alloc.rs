use once_cell::unsync::OnceCell;
use std::alloc::Layout;
use bumpalo::Bump;
use crate::{GcHeader};
use std::mem;
use std::ptr::NonNull;
use std::cell::{Cell, UnsafeCell};
use std::mem::MaybeUninit;

/// The minimum size of supported memory (in words)
///
/// Since the header takes at least one word,
/// its not really worth ever allocating less than this
pub const MINIMUM_WORDS: usize = 2;
/// The maximum words supported by small arenas
///
/// Past this we have to fallback to the global allocator
pub const MAXIMUM_SMALL_WORDS: usize = 32;
/// The alignment of elements in the arena
pub const ARENA_ELEMENT_ALIGN: usize = mem::align_of::<GcHeader>();

#[inline]
pub const fn small_object_size<T>() -> usize {
    let header_layout = Layout::new::<GcHeader>();
    header_layout.size() + header_layout
        .padding_needed_for(std::mem::align_of::<T>())
        + mem::size_of::<T>()
}
#[inline]
pub const fn is_small_object<T>() -> bool {
    small_object_size::<T>() <= MAXIMUM_SMALL_WORDS * 8
        && mem::align_of::<T>() <= ARENA_ELEMENT_ALIGN
}

/// We use zero as our marker value.
///
/// `GcHeader::type_info` is the first field of the header
/// and it will never be null (its a reference).
/// Therefore this marker will never conflict with a valid header.
pub const FREE_SLOT_MARKER: usize = 0;
#[repr(C)]
pub struct FreeSlot {
    /// Marker for the slot, initialized to `FREE_SLOT_MARKER`
    pub marker: usize,
    /// Pointer to the previous free slot
    pub(crate) prev_free: Option<NonNull<MaybeFreeSlot>>,
}
#[repr(C)]
pub(crate) union MaybeFreeSlot {
    pub free: FreeSlot,
    pub header: GcHeader,
}

impl MaybeFreeSlot {
    #[inline]
    pub unsafe fn is_free(&self) -> bool {
        self.free.marker == FREE_SLOT_MARKER
    }
    #[inline]
    pub unsafe fn mark_free(&mut self, prev: Option<NonNull<MaybeFreeSlot>>) {
        debug_assert!(!self.is_free());
        self.free = FreeSlot {
            marker: FREE_SLOT_MARKER,
            prev_free: prev
        };
    }
}
pub const NUM_SMALL_ARENAS: usize = 15;

pub struct SmallArena {
    pub(crate) element_size: usize,
    memory: UnsafeCell<Bump>,
    /// The next free slot
    pub(crate) free: Cell<Option<NonNull<MaybeFreeSlot>>>
}
impl SmallArena {
    #[cold] // Initialization is the slow path
    fn with_words(num_words: usize) -> SmallArena {
        assert!(num_words >= MINIMUM_WORDS);
        let element_size = num_words * mem::size_of::<usize>();
        SmallArena {
            element_size, free: Cell::new(None),
            memory: UnsafeCell::new(Bump::new())
        }
    }
    #[inline]
    pub(crate) fn alloc(&self) -> NonNull<GcHeader> {
        // Check the free list
        if let Some(next_free) = self.free.get() {
            // Update free pointer
            unsafe {
                debug_assert_eq!(next_free.as_ref().free.marker, FREE_SLOT_MARKER);
                self.free.set(next_free.as_ref().free.prev_free);
                NonNull::from(&next_free.as_ref().header)
            }
        } else {
            // Fallback to allocating new memory...
            unsafe {
                let ptr = (*self.memory.get()).alloc_layout(Layout::from_size_align_unchecked(
                    self.element_size, ARENA_ELEMENT_ALIGN
                ));
                ptr.cast::<GcHeader>()
            }
        }
    }
    pub(crate) unsafe fn for_each<F: FnMut(*mut MaybeFreeSlot)>(&self, mut func: F) {
        let iter = (*self.memory.get()).iter_allocated_chunks();
        for chunk in iter {
            let mut ptr = chunk.as_ptr() as *const u8 as *mut u8;
            let end = ptr.add(chunk.len());
            while ptr < end {
                func(ptr as *mut MaybeFreeSlot);
                ptr = ptr.add(self.element_size);
            }
        }
    }
}
macro_rules! arena_match {
    ($arenas:expr, $target:ident, max = $max:expr; $($size:pat => $num_words:literal @ $idx:expr),*) => {
        Some(match $target {
            $($size => $arenas[$idx].get_or_init(|| {
                assert_eq!(SMALL_ARENA_SIZES[$idx], $num_words);
                SmallArena::with_words($num_words)
            }),)*
            _ => {
                assert!($target > $max);
                return None
            }
        })
    };
}
const SMALL_ARENA_SIZES: [usize; NUM_SMALL_ARENAS] =  [
    2, 3, 4, 5, 6, 7, 8,
    10, 12, 14, 16,
    20, 24, 28, 32
];
pub struct SmallArenaList {
    // NOTE: Internally boxed to avoid bloating main struct
    arenas: Box<[OnceCell<SmallArena>; NUM_SMALL_ARENAS]>
}
impl SmallArenaList {
    pub fn new() -> Self {
        // NOTE: Why does writing arrays have to be so difficult:?
        unsafe {
            let mut arenas: Box<[
                MaybeUninit<OnceCell<SmallArena>>;
                NUM_SMALL_ARENAS
            ]> = Box::new_uninit().assume_init();
            for i in 0..NUM_SMALL_ARENAS {
                arenas[i].as_mut_ptr().write(OnceCell::new());
            }
            SmallArenaList {
                // NOTE: This is done becuase I want to explicitly specifiy types
                arenas: mem::transmute::<
                    Box<[MaybeUninit<OnceCell<SmallArena>>; NUM_SMALL_ARENAS]>,
                    Box<[OnceCell<SmallArena>; NUM_SMALL_ARENAS]>
                >(arenas)
            }
        }
    }
    pub fn iter(&self) -> impl Iterator<Item=&SmallArena> + '_ {
        self.arenas.iter().filter_map(OnceCell::get)
    }
    #[inline] // This should be constant folded away (size/align is const)
    pub fn find<T>(&self) -> Option<&SmallArena> {
        if std::mem::align_of::<T>() > ARENA_ELEMENT_ALIGN {
            return None
        }
        // Divide round up
        let word_size = mem::size_of::<usize>();
        let num_words = (small_object_size::<T>() + (word_size - 1))
            / word_size;
        self.find_raw(num_words)
    }
    #[inline(always)] // We want this constant-folded away......
    fn find_raw(&self, num_words: usize) -> Option<&SmallArena> {
        arena_match!(
            self.arenas, num_words, max = 32;
            0..=2 => 2 @ 0,
            3 => 3 @ 1,
            4 => 4 @ 2,
            5 => 5 @ 3,
            6 => 6 @ 4,
            7 => 7 @ 5,
            8 => 8 @ 6,
            9..=10 => 10 @ 7,
            11..=12 => 12 @ 8,
            13..=14 => 14 @ 9,
            15..=16 => 16 @ 10,
            17..=20 => 20 @ 11,
            21..=24 => 24 @ 12,
            25..=28 => 28 @ 13,
            29..=32 => 32 @ 14
        )
    }
}