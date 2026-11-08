//! Running one scene to the end and recording every frame.
//!
//! The field is set up as after a map load (actors placed by their ladders
//! for the story flags, the camera at its defaults), the scene is started the
//! way the game starts it (a type-10 record's talk bit, or the interact
//! button), and the frame loop runs the field tasks in the game's order:
//! Knight → camera → chunk task → Baby → NPCs (drawn here) → script and its
//! children. Each frame is recorded for the player (`encode`).

use serde::Serialize;

use crate::npc::NpcPack;
use crate::world;

use super::math::Tables;
use super::text::{Glyph, Strings, TextPtr};
use super::vm::{ChoiceRec, Ending, Event, Pad, Script};
use super::world::{fl, Actor, Cue, World, BABY, KNIGHT};

/// How the scene starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// The Knight walked into a type-10 record at (x, z).
    Trigger { x: i16, z: i16 },
    /// The Knight pressed the interact button in front of the actor.
    Talk,
}

pub struct SceneInput<'a> {
    pub tables: &'a Tables,
    pub pack: &'a NpcPack,
    pub pack_bytes: &'a [u8],
    pub chunk: u16,
    pub chunk_bytes: &'a [u8],
    /// Actor record whose script runs.
    pub slot: u32,
    pub start: Start,
    /// Story flags set before the scene.
    pub flags: Vec<u16>,
    pub answers: Vec<u8>,
    /// The Baby travels with the Knight.
    pub baby: bool,
    /// Camera yaw (0x1000 = turn); None = behind the Knight, rounded to 45°.
    pub yaw: Option<i16>,
    /// Frames a page end waits for the confirm press: `base + per_glyph · glyphs`.
    pub hold_base: u32,
    pub hold_per_glyph: f32,
    /// `$A`…`$E` (`$F`–`$H` are "Yes", "No", "Save game data?").
    pub names: [String; 5],
    pub item_name: &'a dyn Fn(u16) -> Vec<u8>,
    pub toy_name: &'a dyn Fn(u16) -> Vec<u8>,
    pub max_frames: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayActor {
    /// Character slot (records of kind 4/5 are not actors, so slots are compacted).
    pub slot: u32,
    /// The pack record it was spawned from (what `render3d_actor` takes).
    pub record: u32,
    pub party: bool,
    pub name: String,
    pub model: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundCue {
    pub frame: u32,
    #[serde(flatten)]
    pub cue: Cue,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub frames: u32,
    pub chunk: u16,
    pub slot: u32,
    pub script: u32,
    pub actors: Vec<PlayActor>,
    /// Dialogue pages: glyphs (byte, x, y) in the order they appear.
    pub pages: Vec<Vec<[i16; 3]>>,
    pub speakers: Vec<String>,
    pub menus: Vec<Vec<String>>,
    pub events: Vec<Event>,
    pub cues: Vec<SoundCue>,
    pub presses: Vec<u32>,
    pub choices: Vec<ChoiceRec>,
    pub ending: Ending,
    pub ending_frame: u32,
    pub objective: Option<String>,
    pub flags_before: Vec<u16>,
    pub flags_after: Vec<u16>,
    /// Flags [`play_inferred`] added to `flags_before`, with the reason.
    pub inferred: Vec<Inferred>,
    /// Layer-B attribute (MapId) under the Knight at the start.
    pub attr: u8,
    /// The field music already playing (`bgm/n`, slot 2 cue 1): see [`area_bgm`].
    /// Filled by the caller, which has the zone table.
    pub area_bgm: Option<u16>,
    pub notes: Vec<String>,
    /// The scene is taken out of this game and played from the retail pack, as a preview of
    /// what it did. Filled by the caller, which has that pack.
    pub removed: bool,
}

/// Bytes per frame before the actors, and per actor (see [`encode`]).
pub const FRAME_HEAD: usize = 64 + 4 + 2 + 10 + 4 + 8 + 6 + 16;
pub const ACTOR_BYTES: usize = 12;

pub struct Playback {
    pub meta: Meta,
    pub frames: Vec<u8>,
    /// For [`play_inferred`]: slots the script referred to, flags it cleared
    /// while unset, slots absent after placement, and each slot's first
    /// placing rung flag.
    refs: Vec<usize>,
    cleared: Vec<u16>,
    absent: Vec<bool>,
    placing: Vec<Option<u16>>,
    me: usize,
}

/// A flag added to the story state because the scene needs it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inferred {
    pub flag: u16,
    pub why: String,
}

/// Plays the scene, then once more with the flags it turned out to need:
/// flags its script cleared while they were unset, and the first placing
/// rung of every actor it refers to (and of its own actor) that was absent.
/// `exclude` keeps flags out of the inference.
pub fn play_inferred(inp: &SceneInput, exclude: &[u16]) -> (Playback, Vec<Inferred>) {
    let first = play(inp);
    let mut add: Vec<Inferred> = Vec::new();
    let push = |flag: u16, why: String, add: &mut Vec<Inferred>| {
        if flag != 0 && !inp.flags.contains(&flag) && !exclude.contains(&flag) && !add.iter().any(|a| a.flag == flag) {
            add.push(Inferred { flag, why });
        }
    };
    for &f in &first.cleared {
        push(f, "the scene clears it".into(), &mut add);
    }
    let mut slots = vec![first.me];
    slots.extend(first.refs.iter().copied());
    for s in slots {
        if first.absent.get(s).copied().unwrap_or(false) {
            if let Some(f) = first.placing.get(s).copied().flatten() {
                let name = first.meta.actors.get(s).map(|a| a.name.clone()).unwrap_or_else(|| format!("actor {s}"));
                push(f, format!("places {name}"), &mut add);
            }
        }
    }
    if add.is_empty() {
        return (first, add);
    }
    let mut flags = inp.flags.clone();
    flags.extend(add.iter().map(|a| a.flag));
    let mut second = play(&SceneInput { flags, ..clone_input(inp) });
    second.meta.inferred = add.clone();
    (second, add)
}

fn clone_input<'a>(inp: &SceneInput<'a>) -> SceneInput<'a> {
    SceneInput {
        tables: inp.tables,
        pack: inp.pack,
        pack_bytes: inp.pack_bytes,
        chunk: inp.chunk,
        chunk_bytes: inp.chunk_bytes,
        slot: inp.slot,
        start: inp.start,
        flags: inp.flags.clone(),
        answers: inp.answers.clone(),
        baby: inp.baby,
        yaw: inp.yaw,
        hold_base: inp.hold_base,
        hold_per_glyph: inp.hold_per_glyph,
        names: inp.names.clone(),
        item_name: inp.item_name,
        toy_name: inp.toy_name,
        max_frames: inp.max_frames,
    }
}

/// The field music for a MapId (`MapId_GetAreaId(attr, 1)`, chosen by the
/// field BGM task): byte +0xB of the attribute's 13-byte zone record in table
/// T (`ghost.npc`, `gamedata::encounters`); zones 12 and 14 play `bgm/3`
/// once flag 0xF0 is set; flag 0xBA silences the area music. None = no music.
pub fn area_bgm(t: &[u8], attr: u8, flags: &[u16]) -> Option<u16> {
    if flags.contains(&0xBA) {
        return None;
    }
    let mut r = *t.get(0x28B + attr as usize * 13)? as u16;
    if matches!(attr, 12 | 14) && flags.contains(&0xF0) {
        r = 3;
    }
    (r != 0).then_some(r)
}

/// The condition flag of the first ladder rung that places the actor (not 0, not absent).
fn placing_flag(d: &[u8], ladder: u32) -> Option<u16> {
    let word = |at: usize| if at + 2 <= d.len() { crate::container::le16(d, at) } else { 0 };
    let mut cur = ladder as usize;
    if cur == 0 {
        return None;
    }
    for _ in 0..64 {
        let mut conds = Vec::new();
        while word(cur) & 0xF000 == 0xA000 && conds.len() < 8 {
            conds.push(word(cur) & 0xFFF);
            cur += 2;
        }
        if conds.is_empty() {
            return None;
        }
        let body = word(cur);
        if body != 0x7FFF && body != 10000 {
            return conds.into_iter().find(|&f| f != 0 && f < 512);
        }
        // Skip to the next rung.
        cur += 2;
        while cur + 2 <= d.len() && word(cur) & 0xF000 != 0xA000 {
            cur += 2;
        }
    }
    None
}

/// Kind-4/5 records are torch flames and "objects" the field keeps apart.
fn is_object(kind: u8) -> bool {
    kind == 4 || kind == 5
}

pub fn play(inp: &SceneInput) -> Playback {
    let pack = inp.pack;
    let d = pack.d_block(inp.pack_bytes);
    let heights = world::heightmap(inp.chunk_bytes);
    let mut w = World::new(inp.tables, d, heights, inp.chunk);
    for &f in &inp.flags {
        w.flags.set(f as u32);
    }
    let mut notes = Vec::new();
    if inp.chunk >= 324 {
        // Interiors are entered through doors that set ctx+0xDE bit 0: the 2400 profile.
        w.de |= 1;
        w.cam.profile = [0x960, 0x78, 0x118, 0];
        w.cam.f10 = 0x960;
    }

    // Spawn (`func_801E8E3C` state 1): actor slots skip kind-4/5 records.
    let mut records: Vec<u32> = Vec::new();
    for (ri, r) in pack.actors.iter().enumerate() {
        let kind = (r.d >> 8) as u8;
        if ri >= 2 && is_object(kind) {
            continue;
        }
        let model = (r.b >> 16) as u8;
        let set = (r.b >> 24) as u8;
        let mut a = Actor {
            id: (r.b & 0xFFFF) as u16 as i16,
            model,
            set,
            kind,
            size: r.d as u8,
            script: r.script,
            ladder: r.placement,
            flags: fl::VISIBLE,
            ..Default::default()
        };
        if ri >= 2 && set != 0xFF {
            a.anim_tbl = pack.anim_set(inp.pack_bytes, set as usize);
        }
        records.push(ri as u32);
        w.actors.push(a);
    }
    let Some(me) = records.iter().position(|&r| r == inp.slot) else {
        return empty(inp, "That record is a torch flame or object, not an actor.");
    };
    // Party: the field Knight and Baby (SYS13) with the EXE animation tables.
    for (i, set) in [(KNIGHT, &inp.tables.knight[..6]), (BABY, &inp.tables.baby[..])] {
        let a = &mut w.actors[i];
        a.anim_tbl = set.to_vec();
        a.model = 0;
        a.set = 0;
        a.kind = 0;
        a.size = 2;
        a.id = inp.chunk as i16;
        a.flags = fl::VISIBLE;
    }
    let placing: Vec<Option<u16>> = w.actors.iter().enumerate().map(|(i, a)| if i < 2 { None } else { placing_flag(d, a.ladder) }).collect();
    for i in 2..w.actors.len() {
        w.select_script(i, -1);
        let a = w.actors[i].clone();
        if w.check_npc_at(a.x as i32, a.z as i32, i) != 0 {
            w.actors[i].flags |= fl::OVERLAP;
        }
    }

    let absent: Vec<bool> = w.actors.iter().enumerate().map(|(i, a)| i >= 2 && (a.state == 3 || a.flags & fl::VISIBLE == 0)).collect();
    // Where the Knight stands when it starts.
    let target = w.actors[me].clone();
    let (kx, kz, face) = match inp.start {
        Start::Trigger { x, z } => {
            let f = if me >= 2 { w.t.facing((x as i32, z as i32), (target.x as i32, target.z as i32)) } else { 0 };
            (x as i32, z as i32, f)
        }
        Start::Talk => {
            let (dx, dz) = w.t.polar(target.facing as i32, 0x48);
            let (x, z) = (target.x as i32 + dx, target.z as i32 + dz);
            (x, z, w.t.facing((x, z), (target.x as i32, target.z as i32)))
        }
    };
    let face = ((face + 0x100) & 0xE00) as i16;
    let attr = {
        let a = crate::world::attributes(inp.chunk_bytes);
        let cell = |v: i32| ((v as i16 as i32).clamp(-0x800, 0x7FF) + 0x800) as usize >> 5;
        a.get(cell(kz) * 128 + cell(kx)).copied().unwrap_or(0)
    };
    let mut kh = w.terrain(kx, 0, kz);
    if kh == -1 {
        kh = target.h;
    }
    {
        let k = &mut w.actors[KNIGHT];
        k.x = kx as i16;
        k.z = kz as i16;
        k.h = kh;
        k.facing = face;
        k.target = face;
        k.base = face;
    }
    {
        let (bx, bz) = w.t.polar(face as i32, -0x30);
        let bh = w.terrain(kx + bx, kh as i32, kz + bz);
        let b = &mut w.actors[BABY];
        b.x = (kx + bx) as i16;
        b.z = (kz + bz) as i16;
        b.h = if bh == -1 { kh } else { bh };
        b.facing = face;
        b.target = face;
        if !inp.baby {
            b.flags &= !fl::VISIBLE;
        }
    }
    w.rebuild_trail();
    w.cam.ry = inp.yaw.unwrap_or(((face as i32 + 0x100) & 0xE00) as i16);

    let mut strings = Strings { d, bufs: Vec::new() };
    let mut names = [0u16; 8];
    let extra = ["Yes", "No", "Save game data?"];
    for (k, n) in inp.names.iter().map(|s| s.as_str()).chain(extra).enumerate() {
        names[k] = strings.push(n.as_bytes().to_vec()).src;
    }
    let (base, per) = (inp.hold_base, inp.hold_per_glyph);
    let pad = Pad { hold: Box::new(move |glyphs| base + (glyphs as f32 * per) as u32), answers: inp.answers.clone() };
    let flags_before = w.flags.list();

    let mut rec = Recorder::new(&w);
    let mut script: Option<Script> = None;
    let mut end_frame = None;
    for frame in 0..inp.max_frames {
        w.frame = frame;
        w.knight_update();
        if frame == 0 && inp.start == Start::Talk {
            w.actors[me].flags |= fl::TALK;
            w.actors[KNIGHT].anim = 0;
        }
        w.camera_task();
        // Field_ChunkStreamTask: release the one-frame freeze, then the trigger scan.
        if w.fade == 0 || w.fade == 0x20 {
            w.de &= !0x8000;
        }
        if frame == 0 {
            if let Start::Trigger { .. } = inp.start {
                w.actors[me].flags |= fl::TALK;
                w.de |= 0x8000;
            }
        }
        if inp.baby {
            w.baby_follow();
        }
        w.npc_update();
        rec.capture_world(&w);
        match script.as_mut() {
            None => {
                if w.actors[me].script == 0 {
                    return empty(inp, "The actor has no script.");
                }
                w.de |= 0x80;
                script = Some(Script::new(&mut w, me));
                script.as_mut().unwrap().step(&mut w, &mut strings, &names, inp.item_name, inp.toy_name, &pad);
            }
            Some(s) => s.step(&mut w, &mut strings, &names, inp.item_name, inp.toy_name, &pad),
        }
        let s = script.as_ref().unwrap();
        rec.capture_ui(&w, s, &strings);
        rec.flush(&w);
        if let Some(f) = s.dies_at {
            if frame >= f {
                end_frame = Some(frame);
                break;
            }
        }
    }
    let s = script.unwrap();
    let frames = rec.count;
    let (ending_frame, ending) = match (&s.ending, end_frame) {
        (Some((f, e)), _) => (*f, e.clone()),
        (None, _) => (frames.saturating_sub(1), Ending::Timeout),
    };
    if ending == Ending::Timeout {
        notes.push(format!("Stopped after {} frames without reaching END.", inp.max_frames));
    }
    let actors = records
        .iter()
        .enumerate()
        .map(|(i, &r)| PlayActor {
            slot: i as u32,
            record: r,
            party: i < 2,
            name: match i {
                0 => "Knight".into(),
                1 => "Baby".into(),
                _ => speaker_of(d, w.actors[i].script).unwrap_or_else(|| format!("Actor {r:02}")),
            },
            model: w.actors[i].model != 0xFF,
        })
        .collect();
    let (refs, cleared) = (s.refs.clone(), s.cleared.clone());
    let meta = Meta {
        frames,
        chunk: inp.chunk,
        slot: inp.slot,
        script: w.actors[me].script,
        actors,
        pages: rec.pages.iter().map(|p| p.iter().map(|g| [g.ch as i16, g.x, g.y]).collect()).collect(),
        speakers: rec.speakers,
        menus: rec.menus,
        events: s.events,
        cues: w.cues.iter().map(|&(frame, cue)| SoundCue { frame, cue }).collect(),
        presses: s.presses,
        choices: s.choices,
        ending,
        ending_frame,
        objective: s.objective,
        flags_before,
        flags_after: w.flags.list(),
        inferred: Vec::new(),
        attr,
        area_bgm: None,
        removed: false,
        notes,
    };
    Playback { meta, frames: rec.out, refs, cleared, absent, placing, me }
}

fn empty(inp: &SceneInput, why: &str) -> Playback {
    Playback {
        meta: Meta {
            frames: 0,
            chunk: inp.chunk,
            slot: inp.slot,
            script: 0,
            actors: Vec::new(),
            pages: Vec::new(),
            speakers: Vec::new(),
            menus: Vec::new(),
            events: Vec::new(),
            cues: Vec::new(),
            presses: Vec::new(),
            choices: Vec::new(),
            ending: Ending::Hang { at: 0 },
            ending_frame: 0,
            objective: None,
            flags_before: Vec::new(),
            flags_after: Vec::new(),
            inferred: Vec::new(),
            attr: 0,
            area_bgm: None,
            notes: vec![why.to_string()],
            removed: false,
        },
        frames: Vec::new(),
        refs: Vec::new(),
        cleared: Vec::new(),
        absent: Vec::new(),
        placing: Vec::new(),
        me: 0,
    }
}

/// The first SPEAKER name of a script.
fn speaker_of(d: &[u8], script: u32) -> Option<String> {
    let code = crate::tier2::disassemble(d, script).ok()?;
    code.iter().find_map(|i| match i.operands.first() {
        Some(crate::tier2::Operand::Name(t)) => {
            let n = crate::text::plain(&crate::text::decode(crate::text::c_string(d, *t as usize))).trim().to_string();
            (!n.is_empty()).then_some(n)
        }
        _ => None,
    })
}

/// Collects frames into the binary layout and dedups pages, speakers and menus.
struct Recorder {
    out: Vec<u8>,
    count: u32,
    head: Vec<u8>,
    actors: Vec<u8>,
    pages: Vec<Vec<Glyph>>,
    speakers: Vec<String>,
    menus: Vec<Vec<String>>,
}

fn put_i16(v: &mut Vec<u8>, x: i16) {
    v.extend_from_slice(&x.to_le_bytes());
}

impl Recorder {
    fn new(_w: &World) -> Self {
        Self { out: Vec::new(), count: 0, head: Vec::new(), actors: Vec::new(), pages: Vec::new(), speakers: Vec::new(), menus: Vec::new() }
    }

    /// The camera, fade and every actor as drawn this frame.
    fn capture_world(&mut self, w: &World) {
        let v = &w.view;
        let mut h = Vec::with_capacity(FRAME_HEAD);
        for f in v.r.iter().flatten().chain(v.t.iter()).chain(v.eye.iter()).chain(std::iter::once(&v.h)) {
            h.extend_from_slice(&f.to_le_bytes());
        }
        put_i16(&mut h, v.shake[0]);
        put_i16(&mut h, v.shake[1]);
        h.push(w.fade);
        self.head = h;
        self.actors.clear();
        for a in &w.actors {
            put_i16(&mut self.actors, a.x);
            put_i16(&mut self.actors, a.h);
            put_i16(&mut self.actors, a.z);
            put_i16(&mut self.actors, a.facing);
            let (pa, pb, wt) = a.blend.shown();
            self.actors.push(pa);
            self.actors.push(pb);
            self.actors.push(wt.min(255) as u8);
            let present = a.flags & fl::VISIBLE != 0 && a.state != 3 && a.state != 2 && a.x != 10000 && a.kind & 0xF != 1 && a.flags & fl::NOT_RESIDENT == 0;
            self.actors.push(present as u8 | (a.drawn as u8) << 1);
        }
    }

    /// The dialogue window, bubble, cursor, choice menu and pillar.
    fn capture_ui(&mut self, _w: &World, s: &Script, strings: &Strings) {
        let win = s.window();
        let mut flags = 0u8;
        let (mut wx, mut wy, mut ww, mut wh) = (0, 0, 0, 0);
        if let Some((x, y, wd, ht)) = win.rect {
            flags |= 1;
            (wx, wy, ww, wh) = (x, y, wd, ht);
        }
        let mut page: i16 = -1;
        let glyphs = win.glyphs.len() as u16;
        if !win.glyphs.is_empty() {
            let fits = |p: &Vec<Glyph>| p.iter().zip(&win.glyphs).all(|(a, b)| a == b);
            match self.pages.last_mut() {
                Some(p) if fits(p) => {
                    if win.glyphs.len() > p.len() {
                        *p = win.glyphs.clone();
                    }
                }
                _ => self.pages.push(win.glyphs.clone()),
            }
            page = self.pages.len() as i16 - 1;
        }
        let (mut bx, mut by, mut cx, mut cy) = (0, 0, 0, 0);
        if let Some((x, y)) = win.bubble {
            flags |= 2;
            (bx, by) = (x, y);
        }
        if let Some((x, y)) = win.cursor {
            flags |= 4;
            (cx, cy) = (x, y);
        }
        let mut speaker: i16 = -1;
        if win.bubble.is_some() {
            if let Some(at) = s.speaker {
                let raw = crate::text::c_string(strings.d, at);
                let name = crate::text::plain(&crate::text::decode(raw));
                speaker = match self.speakers.iter().position(|n| *n == name) {
                    Some(i) => i as i16,
                    None => {
                        self.speakers.push(name);
                        self.speakers.len() as i16 - 1
                    }
                };
            }
        }
        let (mut menu, mut mcur) = (-1i16, 0u8);
        if let Some(m) = &s.menu {
            flags |= 8;
            menu = match self.menus.iter().position(|e| *e == m.entries) {
                Some(i) => i as i16,
                None => {
                    self.menus.push(m.entries.clone());
                    self.menus.len() as i16 - 1
                }
            };
            mcur = m.cursor;
        }
        let p = s.pillar.unwrap_or_default();
        if s.pillar.is_some() {
            flags |= 16;
        }
        let h = &mut self.head;
        h.push(flags);
        for v in [wx, wy, ww, wh] {
            put_i16(h, v);
        }
        h.push(win.style);
        h.push(0);
        put_i16(h, page);
        h.extend_from_slice(&glyphs.to_le_bytes());
        for v in [bx, by, cx, cy] {
            put_i16(h, v);
        }
        put_i16(h, speaker);
        put_i16(h, menu);
        h.push(mcur);
        h.push(0);
        for v in [p.x, p.z, p.ground, p.bottom, p.radius, p.spin] {
            put_i16(h, v);
        }
        h.extend_from_slice(&p.dim.to_le_bytes());
        h.push(p.sparkles);
        h.push(p.swirl);
    }

    fn flush(&mut self, _w: &World) {
        debug_assert_eq!(self.head.len(), FRAME_HEAD);
        self.out.extend_from_slice(&self.head);
        self.out.extend_from_slice(&self.actors);
        self.count += 1;
    }
}

/// `TextPtr` is re-exported for callers building messages.
pub type Ptr = TextPtr;
