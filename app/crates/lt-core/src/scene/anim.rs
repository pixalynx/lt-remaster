//! The field's pose sequencer, exactly as `NPC_UpdateActors`
//! (`ovr02_801ebe64.c:251-286`) and `UpdateNpcPoseBlendState`
//! (`main_801897ac.c:2689`) run it each 30 Hz frame:
//!
//! 1. [`step`]: when the blend has landed (weight 0), pre-increment the
//!    frame (wrapping at the count) and make that entry's pose the target
//!    at its speed. After a frame reset, entry 0 is therefore skipped on the
//!    first pass.
//! 2. [`Blend::update`] (only while the actor is drawn on screen): while
//!    current ≠ target, weight += speed; at 0x100 the target becomes current
//!    and the weight returns to 0. An entry lasts `ceil(256/speed)` frames;
//!    one whose pose is already current lasts 1 frame.

/// `Character_Struct+0x1C..0x20`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Blend {
    pub cur: u8,
    pub tgt: u8,
    pub speed: u8,
    pub weight: u8,
}

/// One animation: `(pose, speed)` steps; poses are 1-based (0 = base mesh).
pub type Sequence = Vec<(u8, u8)>;

/// The sequencer half (`+0x30` frame, `+0x14` anim): only between blends.
pub fn step(set: &[Sequence], anim: u8, frame: &mut u8, blend: &mut Blend) {
    if blend.weight != 0 {
        return;
    }
    let Some(seq) = set.get(anim as usize).filter(|s| !s.is_empty()) else { return };
    *frame = frame.wrapping_add(1);
    if *frame as usize >= seq.len() {
        *frame = 0;
    }
    let (pose, speed) = seq[*frame as usize];
    blend.tgt = pose;
    blend.speed = speed;
}

impl Blend {
    /// `UpdateNpcPoseBlendState`.
    pub fn update(&mut self) {
        if self.cur != self.tgt {
            let v = self.weight as u32 + self.speed as u32;
            if v >= 0x100 {
                self.cur = self.tgt;
                self.weight = 0;
            } else {
                self.weight = v as u8;
            }
        }
    }

    /// What the model shows: `(pose a, pose b, weight of b)`. When current
    /// equals target the game skips the blend and keeps the last vertices,
    /// which are pose `cur` in full.
    pub fn shown(&self) -> (u8, u8, u16) {
        if self.cur == self.tgt {
            (self.cur, self.cur, 0)
        } else {
            (self.cur, self.tgt, self.weight as u16)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_breathes_between_two_poses() {
        // npc/6 set 0, animation 0: (0, 13), (1, 13). After a reset (frame 0) the
        // pre-increment picks entry 1 first.
        let set = vec![vec![(0u8, 13u8), (1, 13)]];
        let (mut frame, mut b) = (0u8, Blend::default());
        step(&set, 0, &mut frame, &mut b);
        b.update();
        assert_eq!((frame, b.tgt, b.weight), (1, 1, 13));
        for _ in 0..19 {
            step(&set, 0, &mut frame, &mut b);
            b.update();
        }
        // ceil(256 / 13) = 20 frames: landed on pose 1.
        assert_eq!((b.cur, b.weight), (1, 0));
        step(&set, 0, &mut frame, &mut b);
        b.update();
        assert_eq!((frame, b.tgt, b.weight), (0, 0, 13), "wraps to entry 0");
    }

    #[test]
    fn a_pose_already_shown_lasts_one_frame() {
        let set = vec![vec![(3u8, 86u8), (3, 86), (4, 86)]];
        let (mut frame, mut b) = (0u8, Blend { cur: 3, tgt: 3, ..Default::default() });
        step(&set, 0, &mut frame, &mut b);
        b.update();
        assert_eq!((frame, b.weight), (1, 0), "entry 1 is pose 3, already current");
        step(&set, 0, &mut frame, &mut b);
        b.update();
        assert_eq!((frame, b.tgt, b.weight), (2, 4, 86));
    }
}
