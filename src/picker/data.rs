//! The picker's data: storage, the entries it lists, the selected entry's
//! content and the last deletion (for undo).

use crate::storage::{ClipEntry, EntryId, ListOrder, Storage};
use anyhow::Result;
use std::time::{Duration, Instant};

/// How long a deleted entry can be restored with `u`
pub const UNDO_WINDOW: Duration = Duration::from_secs(5);

/// Deleted entry for undo functionality
pub struct DeletedEntry {
    pub entry: ClipEntry,
    pub content: String,
    pub was_pinned: bool, // Track pin state for restoration
    pub deleted_at: Instant,
}

pub struct PickerData {
    pub storage: Storage,
    /// Pinned first, then most recently used
    pub entries: Vec<ClipEntry>,
    /// Content of the entry shown in the preview pane
    pub content_cache: Option<(EntryId, String)>,
    pub last_deleted: Option<DeletedEntry>,
}

impl PickerData {
    pub fn new(storage: Storage) -> Result<Self> {
        let entries = storage.list(ListOrder::PinnedFirst, None)?;
        Ok(Self { storage, entries, content_cache: None, last_deleted: None })
    }

    /// The cached content, if it is `id`'s.
    pub fn cached_content(&self, id: EntryId) -> Option<&str> {
        self.content_cache.as_ref().filter(|(cached, _)| *cached == id).map(|(_, c)| c.as_str())
    }
}

/// Pinned entries first, each group most recently used first: the order of
/// `ListOrder::PinnedFirst`, so an unpinned entry goes back to its place by
/// recency (not to the top of the unpinned group).
pub fn order_pinned_first(entries: &mut [ClipEntry]) {
    entries.sort_by_key(|e| (!e.pinned, std::cmp::Reverse((e.last_used, e.id))));
}
