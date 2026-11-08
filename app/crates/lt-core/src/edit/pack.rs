//! Everything the project changes inside an NPC pack's D-block, as one set.
//! [`super::text::layout`] lays a set out from retail, so a save depends only
//! on the set, never on the order of the saves. Each encoder reads its part
//! back from the pack's bytes ([`Edits::recover`]), changes it, and lays the
//! whole set out again.

use std::collections::{BTreeMap, BTreeSet};

use super::{actor, character, door, ladder, remove, script, text, Result};

#[derive(Debug, Clone, Default)]
pub struct Edits {
    /// Dialogue lines, retail offset → markup ([`text`]).
    pub text: BTreeMap<u32, String>,
    /// Edited retail programs, retail entry → program ([`script`]).
    pub scripts: script::Scripts,
    /// Trigger records added to table 2 ([`door`]).
    pub doors: Vec<door::Added>,
    /// Added actors, with the programs of their own ([`actor`]).
    pub actors: Vec<actor::AddedActor>,
    /// Retail records taken out of the game ([`remove`]).
    pub removed: BTreeSet<u32>,
    /// Removed smoke records also hidden: drawn no more ([`remove`]).
    pub hidden: BTreeSet<u32>,
    /// Characters brought in from other packs ([`character`]).
    pub characters: Vec<character::Character>,
    /// Retail models and pose blocks dropped to make room ([`character::Freed`]).
    pub freed: character::Freed,
    /// Placement ladders edited per retail slot ([`ladder`]).
    pub ladders: ladder::Ladders,
}

impl Edits {
    /// The set `current` holds over `retail`.
    pub fn recover(retail: &[u8], current: &[u8]) -> Result<Self> {
        let (base, characters, freed) = character::split(retail, current)?;
        let current: &[u8] = &base;
        Ok(Self {
            text: text::edit_set(retail, current)?,
            scripts: script::recover(retail, current)?,
            doors: door::recover(retail, current)?,
            actors: actor::recover(retail, current)?,
            removed: remove::recover(retail, current)?,
            hidden: remove::recover_hidden(retail, current)?,
            ladders: ladder::recover(retail, current)?,
            characters,
            freed,
        })
    }
}

/// Whether `own` is `retail` plus edits these encoders make: it recovers, and laid out again
/// it comes back byte for byte. A blank base's packs are (Living Tool made them), so their
/// edits are read against retail's pack, the reference every encoder lays out against.
pub fn reads_as_edits(retail: &[u8], own: &[u8]) -> bool {
    retail == own || Edits::recover(retail, own).and_then(|set| text::layout(retail, own, &set)).is_ok_and(|(laid, _, _)| laid == own)
}
