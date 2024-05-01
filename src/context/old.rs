use crate::CollectorId;
use zerogc_next_mimalloc_semisafe::heap::MimallocHeap;

pub struct OldGenerationSpace<Id: CollectorId> {
    heap: MimallocHeap,
    collector_id: Id,
    mark_bits_inverted: bool,
}
impl<Id: CollectorId> OldGenerationSpace<Id> {
    #[inline]
    pub fn mark_bits_inverted(&self) -> bool {
        self.mark_bits_inverted
    }
}
