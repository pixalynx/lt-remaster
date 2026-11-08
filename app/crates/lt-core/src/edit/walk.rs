//! Walks the editor writes (LT-95): the words after a placing rung's
//! `x h z dir` that `WalkActor_RunScript` runs every frame
//! ([`crate::scene::world::World::run_script`]).
//!
//! - `8nnn` / `6nnn` set the speed (`6nnn` walks backwards);
//! - `1nnn` heads toward n·512 (eight 45° steps, 0 = north), turning first;
//! - `2nnn` takes n steps of the speed each (`Tables::polar`, truncated);
//! - `0nnn` waits until flag n is set, `7nnn` leaves the map if flag n is set,
//!   `3nnn` sets the animation, `9000` talks, `C/D/Ennn` turn the body;
//! - `0000` stops, `4000` goes back to the first word and `Bnnn` re-places.
//!
//! A rung's walk is read up to its first ending word (or the next condition
//! word), so a walk the editor writes has exactly one ending, last. An actor
//! that stands still has the walk `0000`.

use super::{EditError, Result};

/// Words a walk can have: `placement::decode` reads 64 words of a walk and a
/// copy's branches are read with at most 64 words a rung (`x h z dir` and 60).
pub const MAX_WORDS: usize = 60;
/// Steps one `2nnn` can take: the walker counts them in a byte (actor `+0x36`,
/// `f36 >= n` in `WalkActor_RunScript`), so a `2nnn` over 255 never ends and the
/// actor walks on for good. A longer leg is several `2nnn` back to back.
pub const MAX_STEPS: u16 = 255;
/// The walk of an actor that stands still.
pub const STAND: [u16; 1] = [0x0000];
/// `4000`: back to the first word.
pub const LOOP: u16 = 0x4000;

/// `w` ends a walk: `0000` (stop), `4000` (loop) or `Bnnn` (re-place).
pub const fn ends(w: u16) -> bool {
    w == 0 || w == LOOP || w & 0xF000 == 0xB000
}

/// Checks a walk the editor writes: 1 to [`MAX_WORDS`] words, no condition
/// word (`Annn` ends a rung), loops written `4000`, `2nnn` of at most
/// [`MAX_STEPS`], and one ending, last.
pub fn check(words: &[u16]) -> Result<()> {
    let bad = |m: String| Err(EditError::new("WALK_WORDS", m));
    if words.is_empty() || words.len() > MAX_WORDS {
        return bad(format!("A walk has 1 to {MAX_WORDS} words; this one has {}.", words.len()));
    }
    for (i, &w) in words.iter().enumerate() {
        if w & 0xF000 == 0xA000 {
            return bad(format!("Word {} ({w:04X}) is a branch condition, not a walk word.", i + 1));
        }
        if w & 0xF000 == 0x4000 && w != LOOP {
            return bad(format!("Word {} ({w:04X}): a loop back to the start is written 4000.", i + 1));
        }
        if w & 0xF000 == 0x2000 && w & 0x0FFF > MAX_STEPS {
            return bad(format!("Word {} walks {} steps; the game counts at most {MAX_STEPS} a word, so it would never stop.", i + 1, w & 0x0FFF));
        }
        if ends(w) != (i + 1 == words.len()) {
            return bad(match ends(w) {
                true => format!("Word {} ({w:04X}) ends the walk, so the words after it never run.", i + 1),
                false => "A walk ends with a stop (0000), a loop (4000) or a re-place (Bnnn).".into(),
            });
        }
    }
    Ok(())
}

/// The walk of a placing rung's words (`x h z dir`, then the walk).
pub fn of(words: &[u16]) -> &[u16] {
    words.get(4..).unwrap_or(&[])
}

/// One `2nnn` of a walk as the walker moves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leg {
    /// Index of the `2nnn` word.
    pub word: usize,
    /// Where it ends (x, z).
    pub end: (i32, i32),
    /// What one step moves (x, z): `polar(heading, |speed|)`, backwards at a
    /// negative speed.
    pub step: (i32, i32),
    pub steps: u16,
}

/// Each leg of `words` from `(x, z)` facing `dir`, as the walker moves the
/// actor (`sin·speed/4096` a step, truncated). `speed` is the actor's speed
/// before the walk (a placed actor's is 0). Stops at the ending; turns,
/// waits and the rest don't move it.
pub fn legs(t: &crate::scene::math::Tables, words: &[u16], (x, z): (i32, i32), dir: u16, speed: i32) -> Vec<Leg> {
    let (mut x, mut z, mut heading, mut speed) = (x, z, dir as i32, speed);
    let mut out = Vec::new();
    for (i, &w) in words.iter().enumerate() {
        let arg = (w & 0x0FFF) as i32;
        match w & 0xF000 {
            0x1000 => heading = ((arg << 9) as i16) as i32,
            0x8000 => speed = arg as u8 as i8 as i32,
            0x6000 => speed = arg.wrapping_neg() as u8 as i8 as i32,
            0x2000 => {
                let ang = heading + if speed < 1 { 0x800 } else { 0 };
                let step = t.polar(ang, speed.abs());
                (x, z) = (x + step.0 * arg, z + step.1 * arg);
                out.push(Leg { word: i, end: (x, z), step, steps: arg as u16 });
            }
            _ if ends(w) => break,
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_what_the_editor_writes() {
        assert!(check(&[0x8004, 0x1002, 0x2020, LOOP]).is_ok());
        assert!(check(&STAND).is_ok());
        assert!(check(&[0x8004, 0x2010, 0xB081]).is_ok(), "re-place ends it");
        let code = |w: &[u16]| check(w).unwrap_err().code;
        assert_eq!(code(&[]), "WALK_WORDS");
        assert_eq!(code(&[0x2010]), "WALK_WORDS", "no ending");
        assert_eq!(code(&[0x0000, 0x2010, LOOP]), "WALK_WORDS", "an ending before the end");
        assert_eq!(code(&[0x2010, 0xA081, 0]), "WALK_WORDS", "a condition word");
        assert_eq!(code(&[0x2010, 0x4C54]), "WALK_WORDS", "a loop that isn't 4000 (the copy mark)");
        assert_eq!(code(&[0x8004, 0x2100, 0x0000]), "WALK_WORDS", "256 steps never end (the count is a byte)");
        assert!(check(&[0x8004, 0x20FF, 0x2001, 0x0000]).is_ok(), "255 and 1 more");
        let mut long = vec![0x2001; MAX_WORDS];
        long.push(0);
        assert_eq!(code(&long), "WALK_WORDS", "too long");
        assert!(check(&[0x0081, 0x2010, 0x7004, 0x3002, 0x9000, 0xD800, 0]).is_ok(), "waits, leaves, animations, talk and turns go anywhere");
    }
}
