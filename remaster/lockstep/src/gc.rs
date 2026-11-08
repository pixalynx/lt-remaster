//! Guardian's Crusade's side of the checks: the hook tables and the planted difference.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use lt_emu_hooks::{Action, Call, Hook, Hooks};
use lt_testset::{Image, Machine, Scene};

use crate::Handler;

/// Draw functions in OVR02, guarded by the word OVR02 has there ([`ovr02_words`]).
pub const OVR02_HOOKS: [(&str, u32); 4] = [
    ("WorldChunk_DrawObjects", 0x801E_080C),
    ("WorldChunk_DrawObjectMesh", 0x801E_090C),
    ("Npc_DrawIfVisible", 0x801E_9E90),
    ("Field_DrawTorchFlames", 0x801E_B4AC),
];
/// In main: the model and GTE draw path, the field's chunk streaming, the music sequencer (run
/// from the VSync interrupt) and a battle function.
pub const MAIN_HOOKS: [(&str, u32); 7] = [
    ("Model_DrawParts", 0x8019_DD70),
    ("Model_DrawParts (second entry)", 0x8019_DD68),
    ("Gte_SetComposedTransform", 0x801A_11CC),
    ("Gte_SetRotTransMatrix", 0x801A_1338),
    ("Field_ChunkStreamTask", FIELD_CHUNK_STREAM_TASK),
    ("Sequencer_TickUpdate", 0x8019_A178),
    ("FUN_80144b48 (battle actors)", 0x8014_4B48),
];
pub const FIELD_CHUNK_STREAM_TASK: u32 = 0x8018_B2EC;
pub const WORLD_CHUNK_DRAW_OBJECT_MESH: u32 = 0x801E_090C;
/// `WorldChunk_DrawObjectMesh`'s first word in OVR02 (`sw $s6, 0x204($a3)`).
pub const MESH_FIRST_WORD: u32 = 0xACF6_0204;
/// OVR04's `Camera_UpdateTask` (battle) and its first word; OVR02 has 0 there.
pub const CAMERA_TASK: u32 = 0x801F_D83C;
pub const CAMERA_TASK_OVR04: u32 = 0x27BD_FFD0;
pub const CAMERA_TASK_OVR02: u32 = 0;

/// The Knight's character record (`CHARACTERS[0]`) and gc-sys `Character` offsets.
pub const KNIGHT: u32 = 0x8014_6600;
pub const TALK: u32 = 0x12;
pub const X: u32 = 0x0;

/// The words OVR02 has at [`OVR02_HOOKS`], read from the test set's village scene (the field,
/// OVR02 loaded).
pub fn ovr02_words(image: &Arc<Image>, set: &Path) -> Result<Vec<u32>, String> {
    let (scene, state) = Scene::load(set, "village")?;
    let m = Machine::from_state(image, &scene.card, &state, scene.frame)?;
    let words: Vec<u32> = OVR02_HOOKS.iter().map(|&(_, a)| u32::from_le_bytes(m.read(a, 4).try_into().unwrap())).collect();
    if words[1] != MESH_FIRST_WORD {
        return Err(format!("OVR02 isn't loaded in the village scene: {:#010x} at WorldChunk_DrawObjectMesh", words[1]));
    }
    Ok(words)
}

/// The 13 observe hooks of the whole-set check: [`OVR02_HOOKS`] (guarded), [`MAIN_HOOKS`], and
/// both overlays' words at the battle camera task (only OVR04's fires in battle).
pub fn observe_table(words: &[u32]) -> Vec<Hook> {
    let mut t: Vec<Hook> = OVR02_HOOKS.iter().zip(words).map(|(&(n, a), &w)| Hook::observe(n, a).guarded(w)).collect();
    t.extend(MAIN_HOOKS.iter().map(|&(n, a)| Hook::observe(n, a)));
    t.push(Hook::observe("OVR02 at 0x801FD83C", CAMERA_TASK).guarded(CAMERA_TASK_OVR02));
    t.push(Hook::observe("Camera_UpdateTask (OVR04)", CAMERA_TASK).guarded(CAMERA_TASK_OVR04));
    t
}

/// A side with the observe hooks of [`observe_table`], doing nothing but count.
pub fn observer(words: Vec<u32>) -> impl Fn(&Rc<Cell<u64>>) -> Option<(Hooks, Handler)> {
    move |_| {
        let h: Handler = Box::new(|_: &Call| Action::Continue);
        Some((Hooks::new(observe_table(&words)).expect("table"), h))
    }
}

/// The name of [`planter`]'s hook.
pub const PLANT: &str = "plant (Field_ChunkStreamTask)";

/// A side whose hook on `Field_ChunkStreamTask` xors `bits` into the byte at `at` once, at its
/// first call from frame `from` on; `fired` gets that frame.
pub fn planter(at: u32, bits: u8, from: u64, fired: Rc<Cell<Option<u64>>>) -> impl Fn(&Rc<Cell<u64>>) -> Option<(Hooks, Handler)> {
    move |frame| {
        let (frame, fired) = (frame.clone(), fired.clone());
        let mut done = false;
        let h: Handler = Box::new(move |c: &Call| {
            if !done && frame.get() >= from {
                c.write(at, &[c.read(at, 1)[0] ^ bits]);
                done = true;
                fired.set(Some(frame.get()));
            }
            Action::Continue
        });
        Some((Hooks::new(vec![Hook::observe(PLANT, FIELD_CHUNK_STREAM_TASK)]).expect("table"), h))
    }
}

/// A side that replaces `WorldChunk_DrawObjectMesh`: it returns the packet cursor it was given
/// (`a2`), so no terrain packet is built. What a native terrain renderer does to memory.
pub fn terrain_skipper(_: &Rc<Cell<u64>>) -> Option<(Hooks, Handler)> {
    let h: Handler = Box::new(|c: &Call| Action::Return(Some(c.arg(2))));
    Some((Hooks::new(vec![Hook::replace("WorldChunk_DrawObjectMesh", WORLD_CHUNK_DRAW_OBJECT_MESH).guarded(MESH_FIRST_WORD)]).expect("table"), h))
}
