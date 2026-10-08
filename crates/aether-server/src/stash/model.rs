//! What a rollback recovers, and how it is laid out in a chest window.
//!
//! # What goes in
//!
//! When a rollback undoes a build, the blocks the build was made of do not
//! vanish — they go here, and the player who ordered the rollback can take
//! them back out. The important detail, and the one that is easy to get
//! backwards: the stash receives **what was placed**, not what is standing
//! there now.
//!
//! Those differ whenever someone has built on top of the thing being undone.
//! Undoing Alice's cobblestone tower after Bob has clad it in glass must hand
//! back Alice's *cobblestone* — that is what the rollback took away. Reading
//! the current world would hand back Bob's glass, which is both wrong and a
//! duplication: Bob's glass still exists, three blocks over.
//!
//! Each entry therefore comes from the event's `to` field — the state the
//! rolled-back player put there — together with whatever tags that state
//! carried.
//!
//! # Grouping
//!
//! A thousand undone cobblestone blocks are one line, not a thousand. Entries
//! are grouped by *(block name, tags)*, so two stacks that differ only in a
//! tag stay apart while a thousand identical ones collapse into a single slot
//! showing the full count — which is larger than a stack, and deliberately so:
//! the window is a manifest, not an inventory. Withdrawal splits it into real
//! stacks.

use std::collections::BTreeMap;

/// One kind of recovered block, with everything that distinguishes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Registry name, e.g. `minecraft:cobblestone`.
    pub block: String,
    /// The block state's properties, as `key=value`, sorted — the "tags" that
    /// keep two otherwise-identical blocks apart. Sorted so that grouping is
    /// stable regardless of the order the properties arrived in.
    pub tags: Vec<String>,
    /// How many were recovered. Not capped at a stack.
    pub count: u64,
}

impl Entry {
    /// The grouping key: two entries merge exactly when this matches.
    fn key(&self) -> (String, Vec<String>) {
        (self.block.clone(), self.tags.clone())
    }

    /// The slot label: name and tags, without the count.
    ///
    /// The count is appended separately by
    /// [`crate::protocol::ContainerSlot::with_quantity`], and only when the
    /// stack-count badge cannot show it — so it appears exactly once either
    /// way.
    pub fn short_label(&self) -> String {
        let short = self.block.split_once(':').map_or(&*self.block, |(_, n)| n);
        if self.tags.is_empty() {
            short.to_owned()
        } else {
            format!("{short} [{}]", self.tags.join(","))
        }
    }

    /// A one-line description for chat and for the item's hover text.
    pub fn describe(&self) -> String {
        if self.tags.is_empty() {
            format!("{} x{}", self.block, self.count)
        } else {
            format!("{} [{}] x{}", self.block, self.tags.join(","), self.count)
        }
    }
}

/// Everything one player has recovered and not yet taken back.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Stash {
    entries: Vec<Entry>,
}

impl Stash {
    /// Add recovered blocks, merging into an existing entry where the block
    /// and tags match.
    pub fn deposit(&mut self, block: &str, tags: &[String], count: u64) {
        if count == 0 || block == "minecraft:air" {
            // Undoing a placement recovers the block that was placed; undoing
            // a *break* recovers nothing, because nothing was placed. Letting
            // air in would fill the window with meaningless slots.
            return;
        }
        let mut tags = tags.to_vec();
        tags.sort();
        if let Some(e) = self
            .entries
            .iter_mut()
            .find(|e| e.block == block && e.tags == tags)
        {
            e.count = e.count.saturating_add(count);
            return;
        }
        self.entries.push(Entry {
            block: block.to_owned(),
            tags,
            count,
        });
    }

    /// Merge a batch, then order the result: most numerous first, ties broken
    /// by name so the window does not reshuffle between openings.
    pub fn deposit_all(&mut self, items: impl IntoIterator<Item = (String, Vec<String>, u64)>) {
        for (block, tags, count) in items {
            self.deposit(&block, &tags, count);
        }
        self.sort();
    }

    fn sort(&mut self) {
        self.entries
            .sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key().cmp(&b.key())));
    }

    /// Take up to `count` of entry `index`, returning how many were actually
    /// taken. An emptied entry is removed, which is what keeps the window from
    /// filling with zeroes.
    pub fn take(&mut self, index: usize, count: u64) -> u64 {
        let Some(e) = self.entries.get_mut(index) else {
            return 0;
        };
        let taken = count.min(e.count);
        e.count -= taken;
        if e.count == 0 {
            self.entries.remove(index);
        }
        taken
    }

    /// Every entry, in display order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Number of distinct entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether anything is stashed.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The total number of blocks held, across all entries.
    pub fn total(&self) -> u64 {
        self.entries.iter().map(|e| e.count).sum()
    }

    /// Build a stash from a rollback plan.
    ///
    /// `placed_by_rollback` is what each undone event *put* there — the `to`
    /// side — which is what the rollback removed and therefore what is owed
    /// back.
    pub fn from_recovered(items: impl IntoIterator<Item = (String, Vec<String>)>) -> Stash {
        let mut counts: BTreeMap<(String, Vec<String>), u64> = BTreeMap::new();
        for (block, mut tags) in items {
            tags.sort();
            *counts.entry((block, tags)).or_default() += 1;
        }
        let mut s = Stash {
            entries: counts
                .into_iter()
                .map(|((block, tags), count)| Entry { block, tags, count })
                .collect(),
        };
        s.sort();
        s
    }
}

// ---------------------------------------------------------------------------
// Window layout
// ---------------------------------------------------------------------------

/// A double chest: six rows of nine.
pub const ROWS: usize = 6;
/// Slots per row.
pub const COLS: usize = 9;
/// Total slots in the container part of the window.
pub const SLOTS: usize = ROWS * COLS;

/// Slot of the "previous page" arrow.
pub const SLOT_PREV: usize = 45 + 3;
/// Slot of the "next page" arrow.
pub const SLOT_NEXT: usize = 45 + 5;

/// What occupies one slot of the open window.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    /// Grey stained glass: the frame. Clicking does nothing.
    Filler,
    /// One stash entry, at this index into [`Stash::entries`].
    Item {
        index: usize,
    },
    /// Page back / page forward.
    PrevPage,
    NextPage,
}

/// The interior slots, in reading order: everything inside the one-slot border,
/// minus the bottom row that holds the arrows.
///
/// A 9x6 window with a border leaves a 7x4 interior — 28 entries a page.
pub fn interior_slots() -> Vec<usize> {
    let mut out = Vec::with_capacity(28);
    for row in 1..ROWS - 1 {
        for col in 1..COLS - 1 {
            out.push(row * COLS + col);
        }
    }
    out
}

/// Entries per page.
pub fn per_page() -> usize {
    interior_slots().len()
}

/// Lay out `page` (0-based) of `stash` as one cell per window slot.
pub fn layout(stash: &Stash, page: usize) -> Vec<Cell> {
    let mut cells = vec![Cell::Filler; SLOTS];
    let interior = interior_slots();
    let start = page * interior.len();
    for (n, slot) in interior.iter().enumerate() {
        match stash.entries().get(start + n) {
            Some(_) => cells[*slot] = Cell::Item { index: start + n },
            None => break,
        }
    }
    // Arrows only where there is somewhere to go: an arrow that does nothing
    // is worse than no arrow, because it reads as a bug.
    if page > 0 {
        cells[SLOT_PREV] = Cell::PrevPage;
    }
    if start + interior.len() < stash.len() {
        cells[SLOT_NEXT] = Cell::NextPage;
    }
    cells
}

/// Total pages for a stash, at least one.
pub fn pages(stash: &Stash) -> usize {
    stash.len().div_ceil(per_page()).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags<const N: usize>(v: [&str; N]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn identical_blocks_collapse_into_one_entry() {
        let mut s = Stash::default();
        for _ in 0..1000 {
            s.deposit("minecraft:cobblestone", &[], 1);
        }
        assert_eq!(s.len(), 1);
        assert_eq!(s.entries()[0].count, 1000, "not capped at a stack");
    }

    #[test]
    fn blocks_that_differ_only_in_a_tag_stay_apart() {
        // The whole reason tags are part of the key: an oak stair facing north
        // is not an oak stair facing south, and merging them loses the build.
        let mut s = Stash::default();
        s.deposit("minecraft:oak_stairs", &tags(["facing=north"]), 4);
        s.deposit("minecraft:oak_stairs", &tags(["facing=south"]), 2);
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn tag_order_does_not_split_a_group() {
        let mut s = Stash::default();
        s.deposit(
            "minecraft:oak_stairs",
            &tags(["facing=north", "half=top"]),
            1,
        );
        s.deposit(
            "minecraft:oak_stairs",
            &tags(["half=top", "facing=north"]),
            1,
        );
        assert_eq!(
            s.len(),
            1,
            "the same tags in another order are the same tags"
        );
        assert_eq!(s.entries()[0].count, 2);
    }

    #[test]
    fn air_is_never_stashed() {
        // Undoing a *break* recovers nothing: nothing was placed. Without this
        // every rollback of a mined tunnel would hand back a window full of
        // air.
        let mut s = Stash::default();
        s.deposit("minecraft:air", &[], 500);
        assert!(s.is_empty());
    }

    #[test]
    fn taking_removes_an_emptied_entry_and_clamps_an_overdraw() {
        let mut s = Stash::default();
        s.deposit("minecraft:stone", &[], 10);
        assert_eq!(s.take(0, 3), 3);
        assert_eq!(s.entries()[0].count, 7);
        assert_eq!(s.take(0, 999), 7, "cannot take more than is there");
        assert!(s.is_empty(), "an emptied entry leaves the window");
        assert_eq!(s.take(0, 1), 0, "taking from nothing is not an error");
    }

    #[test]
    fn the_border_is_a_frame_and_the_interior_is_seven_by_four() {
        let interior = interior_slots();
        assert_eq!(interior.len(), 28);
        // No interior slot may touch an edge.
        for s in &interior {
            let (row, col) = (s / COLS, s % COLS);
            assert!(
                row > 0 && row < ROWS - 1,
                "slot {s} is on a horizontal edge"
            );
            assert!(col > 0 && col < COLS - 1, "slot {s} is on a vertical edge");
        }
    }

    #[test]
    fn an_empty_stash_is_all_frame_and_no_arrows() {
        let cells = layout(&Stash::default(), 0);
        assert_eq!(cells.len(), SLOTS);
        assert!(cells.iter().all(|c| *c == Cell::Filler));
    }

    #[test]
    fn arrows_appear_only_where_there_is_somewhere_to_go() {
        let mut s = Stash::default();
        for i in 0..per_page() * 2 + 1 {
            s.deposit(&format!("minecraft:block_{i}"), &[], 1);
        }
        let first = layout(&s, 0);
        assert_eq!(first[SLOT_PREV], Cell::Filler, "no back arrow on page one");
        assert_eq!(first[SLOT_NEXT], Cell::NextPage);

        let last = layout(&s, pages(&s) - 1);
        assert_eq!(last[SLOT_PREV], Cell::PrevPage);
        assert_eq!(
            last[SLOT_NEXT],
            Cell::Filler,
            "no forward arrow on the last"
        );
    }

    #[test]
    fn every_entry_appears_on_exactly_one_page() {
        // The failure this catches is an off-by-one in the page offset, which
        // would silently make some recovered blocks unreachable.
        let mut s = Stash::default();
        let n = per_page() * 3 + 5;
        for i in 0..n {
            s.deposit(&format!("minecraft:block_{i:03}"), &[], 1);
        }
        let mut seen = std::collections::HashSet::new();
        for page in 0..pages(&s) {
            for cell in layout(&s, page) {
                if let Cell::Item { index } = cell {
                    assert!(seen.insert(index), "entry {index} shown on two pages");
                }
            }
        }
        assert_eq!(seen.len(), n, "every entry must be reachable");
    }

    #[test]
    fn a_rollback_plan_is_counted_by_block_and_tags() {
        let s = Stash::from_recovered([
            ("minecraft:stone".to_string(), vec![]),
            ("minecraft:stone".to_string(), vec![]),
            ("minecraft:oak_stairs".to_string(), tags(["facing=north"])),
        ]);
        assert_eq!(s.total(), 3);
        assert_eq!(s.len(), 2);
        // Most numerous first.
        assert_eq!(s.entries()[0].block, "minecraft:stone");
        assert_eq!(s.entries()[0].count, 2);
    }
}

// ---------------------------------------------------------------------------
// The quantity dialogue
// ---------------------------------------------------------------------------

/// The steps the adjust buttons move by, largest first.
///
/// One, sixteen, half a stack and a whole stack. Four rather than a text box
/// because a container window has no text input, and these four reach any
/// round number a player actually wants in two or three clicks: 64 for a
/// stack, 64+16 for the odd chest row, 1 for the last one.
pub const STEPS: [u64; 4] = [64, 32, 16, 1];

/// Row the adjust buttons sit on: the middle of the window.
const ADJUST_ROW: usize = 2;
/// Row the confirm and cancel buttons sit on.
const DECIDE_ROW: usize = 4;

/// Slot showing the item and the amount currently chosen — the centre of the
/// adjust row, with the four decreases to its left and the four increases to
/// its right, in mirrored magnitude order.
pub const SLOT_AMOUNT: usize = ADJUST_ROW * COLS + 4;
/// Slot of the cancel button.
pub const SLOT_CANCEL: usize = DECIDE_ROW * COLS + 2;
/// Slot of the confirm button.
pub const SLOT_CONFIRM: usize = DECIDE_ROW * COLS + 6;

/// What occupies one slot of the quantity dialogue.
#[derive(Debug, Clone, PartialEq)]
pub enum AskCell {
    Filler,
    /// Change the amount by this much. Negative decreases.
    Adjust(i64),
    /// The item, showing the amount currently chosen.
    Amount,
    Cancel,
    Confirm,
}

/// Lay out the quantity dialogue.
///
/// The adjust row reads `-64 -32 -16 -1 [item] +1 +16 +32 +64`: mirrored around
/// the amount, magnitudes growing outwards, so the pair of buttons for a given
/// step are the same distance from the centre on either side.
///
/// **Every button is always present.** A `+64` with 2 of 100 left does not
/// disappear — it tops up to 100, and a `-64` at 10 goes to 1. Hiding them was
/// the wrong call: a row whose buttons come and go as the number changes is
/// one a player has to re-read on every click, and the clamped behaviour is
/// what they meant by pressing it anyway.
pub fn ask_layout(_chosen: u64, _available: u64) -> Vec<AskCell> {
    let mut cells = vec![AskCell::Filler; SLOTS];
    for (n, step) in STEPS.iter().enumerate() {
        // n = 0 is the largest step and sits furthest out.
        cells[ADJUST_ROW * COLS + n] = AskCell::Adjust(-(*step as i64));
        cells[ADJUST_ROW * COLS + (COLS - 1 - n)] = AskCell::Adjust(*step as i64);
    }
    cells[SLOT_AMOUNT] = AskCell::Amount;
    cells[SLOT_CANCEL] = AskCell::Cancel;
    cells[SLOT_CONFIRM] = AskCell::Confirm;
    cells
}

/// Apply an adjust button, clamped to `1..=available`.
///
/// Clamping is the whole behaviour, not an edge case: `+64` with 98 of 100
/// means 100, and `-64` at 10 means 1. A player pressing a button that would
/// overshoot wants the end of the range, not a rejected click and not zero.
/// Zero is reachable only by cancelling.
pub fn adjust(chosen: u64, delta: i64, available: u64) -> u64 {
    // Done in `u64` throughout. Going via `i64` would silently cap the ceiling
    // at half the range, which is invisible until a count above that shows up
    // — and counts here come from a rollback plan, bounded by the world rather
    // than by anything this module controls.
    let moved = if delta >= 0 {
        chosen.saturating_add(delta as u64)
    } else {
        chosen.saturating_sub(delta.unsigned_abs())
    };
    moved.clamp(1, available.max(1))
}

/// The amount the dialogue opens on: a stack, or everything if that is less.
pub fn default_take(available: u64) -> u64 {
    available.clamp(1, 64)
}

#[cfg(test)]
mod ask_tests {
    use super::*;

    #[test]
    fn the_adjust_row_is_mirrored_around_the_amount() {
        let cells = ask_layout(100, 1000);
        let row: Vec<&AskCell> = cells[ADJUST_ROW * COLS..ADJUST_ROW * COLS + COLS]
            .iter()
            .collect();
        assert_eq!(row[0], &AskCell::Adjust(-64));
        assert_eq!(row[1], &AskCell::Adjust(-32));
        assert_eq!(row[2], &AskCell::Adjust(-16));
        assert_eq!(row[3], &AskCell::Adjust(-1));
        assert_eq!(row[4], &AskCell::Amount);
        assert_eq!(row[5], &AskCell::Adjust(1));
        assert_eq!(row[6], &AskCell::Adjust(16));
        assert_eq!(row[7], &AskCell::Adjust(32));
        assert_eq!(row[8], &AskCell::Adjust(64));
    }

    #[test]
    fn there_are_exactly_eight_adjust_buttons() {
        // Four down and four up, as asked for: stack, half a stack, sixteen,
        // one.
        let cells = ask_layout(100, 1000);
        let ups = cells
            .iter()
            .filter(|c| matches!(c, AskCell::Adjust(d) if *d > 0))
            .count();
        let downs = cells
            .iter()
            .filter(|c| matches!(c, AskCell::Adjust(d) if *d < 0))
            .count();
        assert_eq!((ups, downs), (4, 4));
        assert_eq!(STEPS, [64, 32, 16, 1]);
    }

    #[test]
    fn the_buttons_never_move_however_much_is_chosen() {
        // A row that changes shape as the number changes is one the player has
        // to re-read on every click.
        let shape = |chosen, available| -> Vec<AskCell> {
            ask_layout(chosen, available)[ADJUST_ROW * COLS..ADJUST_ROW * COLS + COLS].to_vec()
        };
        let full = shape(50, 100);
        assert_eq!(shape(1, 100), full, "at the minimum");
        assert_eq!(shape(100, 100), full, "at the maximum");
        assert_eq!(shape(1, 1), full, "with nothing to adjust at all");
    }

    #[test]
    fn a_button_that_would_overshoot_tops_up_to_the_limit() {
        // The case the user named: 98 of 100, press +64, get 100 — not a
        // hidden button and not a refused click.
        assert_eq!(adjust(98, 64, 100), 100);
        assert_eq!(adjust(100, 64, 100), 100, "already there, stays there");
        assert_eq!(adjust(10, -64, 1000), 1, "the floor is one, never zero");
        assert_eq!(adjust(1, -1, 1000), 1);
    }

    #[test]
    fn adjusting_moves_by_the_step_when_there_is_room() {
        assert_eq!(adjust(70, -64, 1000), 6);
        assert_eq!(adjust(900, 64, 1000), 964);
    }

    #[test]
    fn adjusting_a_count_near_the_top_of_the_range_does_not_wrap() {
        // Counts come from a rollback plan, so they are bounded by the world,
        // not by anything this module controls.
        assert_eq!(adjust(u64::MAX, 64, u64::MAX), u64::MAX);
        assert_eq!(adjust(1, -64, u64::MAX), 1);
    }

    #[test]
    fn the_dialogue_opens_on_a_stack_or_on_everything_if_that_is_less() {
        assert_eq!(default_take(1000), 64);
        assert_eq!(default_take(64), 64);
        assert_eq!(default_take(3), 3);
        assert_eq!(default_take(0), 1, "never opens on an unconfirmable zero");
    }

    #[test]
    fn confirm_and_cancel_never_share_a_slot_with_an_adjust_button() {
        // They are on a different row entirely, but a layout change that moved
        // one onto the other would silently make a click do two things.
        let cells = ask_layout(50, 100);
        assert_eq!(cells[SLOT_CONFIRM], AskCell::Confirm);
        assert_eq!(cells[SLOT_CANCEL], AskCell::Cancel);
        assert!(matches!(cells[SLOT_AMOUNT], AskCell::Amount));
        assert_ne!(SLOT_CONFIRM, SLOT_CANCEL);
        for slot in [SLOT_CONFIRM, SLOT_CANCEL] {
            assert!(slot / COLS != ADJUST_ROW, "decide row overlaps adjust row");
        }
    }

    #[test]
    fn cancel_and_confirm_are_always_offered() {
        // The amount can never reach zero — `adjust` floors it at one — so
        // confirm is never the meaningless button it would otherwise be.
        for (chosen, available) in [(1u64, 1u64), (50, 100), (100, 100)] {
            let cells = ask_layout(chosen, available);
            assert_eq!(cells[SLOT_CANCEL], AskCell::Cancel);
            assert_eq!(cells[SLOT_CONFIRM], AskCell::Confirm);
        }
    }
}
