//! The dialogue window, ported line by line from `Dialogue_DisplayTextTask`
//! (`ovr02.c:1369-1623`), its window animation `FUN_OVR2__801e40f8`
//! (`:1267-1365`), the speaker bubble (`:1160-1226`) and the bounce cursor
//! (`:3193-3214`).
//!
//! Every frame the page is redrawn from its start, one more unit (a glyph,
//! a newline, a box prefix or one character of an inserted name) than the
//! frame before; a confirm press at a page end advances. The window opens
//! over 2 invisible frames (3 on a reopen) and 4 growing ones, and is drawn
//! at 228×48 once open (232×52 on the frame it finishes opening).

/// Where text is read from: the pack's D-block or one of the player's buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPtr {
    /// 0 = D-block, else `Strings::bufs[src − 1]`.
    pub src: u16,
    pub at: usize,
}

/// The bytes text can come from: the D-block, CHEST messages, and the
/// strings `$A`…`$H`, `#ddd` and `~dd` insert.
pub struct Strings<'a> {
    pub d: &'a [u8],
    pub bufs: Vec<Vec<u8>>,
}

impl Strings<'_> {
    pub fn byte(&self, p: TextPtr, k: usize) -> u8 {
        let b: &[u8] = if p.src == 0 { self.d } else { self.bufs.get(p.src as usize - 1).map(|v| v.as_slice()).unwrap_or(&[]) };
        b.get(p.at + k).copied().unwrap_or(0)
    }

    /// Adds a NUL-terminated buffer and returns a pointer to its start.
    pub fn push(&mut self, mut bytes: Vec<u8>) -> TextPtr {
        if bytes.last() != Some(&0) {
            bytes.push(0);
        }
        self.bufs.push(bytes);
        TextPtr { src: self.bufs.len() as u16, at: 0 }
    }
}

/// One glyph drawn this frame: byte, pen x, pen y (320×224 screen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    pub ch: u8,
    pub x: i16,
    pub y: i16,
}

/// The window slot (`MenuWin`, text-task view of it).
#[derive(Debug, Clone, Copy, Default)]
pub struct Win {
    pub x: i16,
    pub y: i16,
    /// Last box code (frame tile column).
    pub style: u8,
    /// 1 opening, 0 open, 2 closing.
    pub fd: u8,
    /// Open/close step (s8: −1 after a close).
    pub count: i8,
    /// 0x8000 visible, 0x100 open SFX played, 0x80 bubble spawned, 0x1000 inserting.
    pub flags: u16,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Bubble {
    pub tick: u32,
    pub state: u8,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Cursor {
    /// `t->time` (vblanks: 1 on tick 0, then 2 a frame).
    pub time: i32,
    pub flip: bool,
    pub tick: u32,
}

/// What the window shows this frame.
#[derive(Debug, Clone, Default)]
pub struct WinFrame {
    /// Drawn window rectangle and frame style.
    pub rect: Option<(i16, i16, i16, i16)>,
    pub style: u8,
    pub glyphs: Vec<Glyph>,
    /// Speaker bubble top-left.
    pub bubble: Option<(i16, i16)>,
    /// Bounce cursor position.
    pub cursor: Option<(i16, i16)>,
}

#[derive(Debug, Clone, Default)]
pub struct TextTask {
    pub tick: u32,
    /// `f29`: 0 typing · 1 SAY3 done · 2 idle/closing · 3 SAY2 done · 6 waiting for confirm.
    pub state: u8,
    /// `f2A`: units to draw this frame − 1.
    pub units: u8,
    /// `f2`: 1 on the first frame of a page.
    pub page_start: i16,
    /// SAY variant (1/2/3).
    pub arg: u8,
    pub win: Win,
    pub bubble: Option<Bubble>,
    pub cursor: Option<Cursor>,
    pub out: WinFrame,
}

/// What the text task reads and writes outside itself.
pub struct TextEnv<'s, 'a> {
    /// `D_801FD630`.
    pub text: &'s mut Option<TextPtr>,
    /// `D_801FD5B0` (the speaker name's D offset).
    pub speaker: Option<usize>,
    pub confirm: bool,
    /// `ctx+0x4C`.
    pub mode4c: u8,
    pub knight_flags: &'s mut u8,
    /// The script's `f4` (child signal).
    pub signal: &'s mut u32,
    pub strings: &'s mut Strings<'a>,
    /// `$A`…`$H` buffer indices into `strings` (1-based src).
    pub names: &'s [u16; 8],
    pub item_name: &'s dyn Fn(u16) -> Vec<u8>,
    pub toy_name: &'s dyn Fn(u16) -> Vec<u8>,
    pub sfx: &'s mut Vec<u16>,
}

impl TextTask {
    pub fn new() -> Self {
        Self { state: 2, win: Win { x: 0x34, y: 0xA4, fd: 1, ..Default::default() }, ..Default::default() }
    }

    /// `FUN_OVR2__801e40f8`: one open/close step and the window drawn this frame.
    fn win_anim(&mut self, mode4c: u8) -> i32 {
        let w = &mut self.win;
        let count = w.count as i32 - 1;
        let (mut width, mut height) = (count * 0x26, count * 8);
        if mode4c != 0 {
            return mode4c as i32;
        }
        let mut result = 0;
        if w.fd == 1 {
            let old = w.count;
            w.count = w.count.wrapping_add(1);
            if old < 6 {
                if w.count < 3 {
                    return 0;
                }
            } else {
                w.fd = 0;
                width = 0xE8;
                height = 0x34;
            }
        } else if w.fd == 2 {
            let old = w.count;
            w.count = w.count.wrapping_sub(1);
            if old <= 0 {
                result = -2;
            }
        }
        if result == 0 {
            let left = (0x140 - width) / 2;
            let top = w.y as i32 + (0x34 - height) / 2;
            self.out.rect = Some((left as i16, top as i16, width as i16, height as i16));
            self.out.style = w.style;
        }
        result
    }

    /// One frame of the task (the script is its parent).
    pub fn step(&mut self, env: &mut TextEnv) {
        self.out = WinFrame::default();
        self.run(env);
        // Children: the bubble, then the cursor (spawn order varies; both only draw).
        self.step_bubble(env);
        self.step_cursor();
        self.tick += 1;
    }

    fn run(&mut self, env: &mut TextEnv) {
        if self.tick == 0 {
            self.win = Win { x: 0x34, y: 0xA4, fd: 1, ..Default::default() };
            self.arg = 0;
            self.state = 2;
        }
        let mut pen = (self.win.x + 0x14, self.win.y + 8);
        if self.state == 0 && env.confirm {
            self.units = 100;
        }
        let mut text = *env.text;
        *env.signal = 0;
        self.win.flags &= 0xEFFF;
        if env.speaker.is_none() {
            self.win.flags &= 0xEF7F;
        }
        if self.win.flags & 0x8000 != 0 {
            if self.win.flags & 0x100 == 0 {
                env.sfx.push(if self.win.style == 3 { 9 } else { 8 });
                self.win.flags |= 0x100;
            }
            if self.win_anim(env.mode4c) == -2 {
                self.win.flags &= 0x7EFF;
                if self.state == 2 {
                    *env.signal = 2;
                }
                return;
            }
            if self.win.count < 7 {
                return;
            }
        }
        if (self.state as i32 - 2) as u32 & 0xFF < 2 && matches!(self.state, 2 | 3) {
            if text.is_none() {
                return;
            }
            *env.knight_flags |= 1;
            self.state = 0;
            self.units = 0;
            if env.speaker.is_some() && self.win.flags & 0x80 == 0 {
                self.win.flags |= 0x80;
                self.bubble = Some(Bubble::default());
            }
        }
        if self.state == 1 && text.is_none() {
            self.state = 3;
            return;
        }
        let Some(mut p) = text else { return };
        let mut resume = p;
        let mut i: u32 = 0;
        let mut jumped_to_after = false;
        'units: while i <= self.units as u32 {
            let flags = self.win.flags;
            let mut handled = false;
            if flags & 0x1000 == 0 {
                match env.strings.byte(p, 0) {
                    0 => {
                        if self.state == 0 {
                            if self.arg == 2 {
                                self.cursor = Some(Cursor::default());
                                self.page_start = 0;
                            }
                            self.state = 6;
                        }
                        if i == 0 {
                            self.state = 2;
                            self.win.fd = 2;
                            self.page_start = 1;
                        }
                        break 'units;
                    }
                    4 | 5 => {
                        if env.strings.byte(p, 0) == 4 {
                            p.at += 1;
                            if self.units != 0 {
                                return;
                            }
                            self.win.fd = 2;
                            *env.text = Some(p);
                        }
                        let code = env.strings.byte(p, 1) as u16 | (env.strings.byte(p, 2) as u16) << 8;
                        if code == 5 || code == 3 {
                            if self.win.fd == 0 {
                                self.win.fd = 2;
                                return;
                            }
                            self.win.y = if code == 5 { 0x5A } else { 0x10 };
                        } else {
                            if (self.win.style == 3 || self.win.style == 5) && self.win.fd == 0 {
                                self.win.fd = 2;
                                return;
                            }
                            self.win.y = 0xA4;
                        }
                        self.win.x = 0x34;
                        p.at += 3;
                        handled = true;
                        self.win.style = code as u8;
                        *env.text = Some(p);
                    }
                    6 => {
                        if self.state != 0 {
                            jumped_to_after = true;
                            break 'units;
                        }
                        self.cursor = Some(Cursor::default());
                        self.state = 6;
                        break 'units;
                    }
                    7 => {
                        handled = true;
                        pen.0 = self.win.x + 0x14;
                        pen.1 += 0xC;
                    }
                    b'$' => {
                        self.win.flags = flags | 0x1000;
                        resume = TextPtr { src: p.src, at: p.at + 2 };
                        let idx = (env.strings.byte(p, 1) as i32 - 0x41).clamp(0, 7) as usize;
                        p = TextPtr { src: env.names[idx], at: 0 };
                    }
                    b'#' => {
                        self.win.flags = flags | 0x1000;
                        let dig = |k: usize| env.strings.byte(p, k) as i32 - b'0' as i32;
                        let mut n = dig(1) * 100 + dig(2) * 10 + dig(3);
                        resume = TextPtr { src: p.src, at: p.at + 4 };
                        if n >= 500 {
                            n -= 302;
                        }
                        let name = (env.item_name)(n.max(0) as u16);
                        p = env.strings.push(name);
                    }
                    b'~' => {
                        self.win.flags = flags | 0x1000;
                        let dig = |k: usize| env.strings.byte(p, k) as i32 - b'0' as i32;
                        let n = dig(1) * 10 + dig(2);
                        resume = TextPtr { src: p.src, at: p.at + 3 };
                        let name = (env.toy_name)(n.max(0) as u16);
                        p = env.strings.push(name);
                    }
                    _ => {}
                }
            }
            if !handled {
                if self.win.flags & 0x8000 != 0 {
                    let ch = env.strings.byte(p, 0);
                    if ch == b' ' {
                        pen.0 += 8;
                    } else {
                        self.out.glyphs.push(Glyph { ch, x: pen.0, y: pen.1 });
                        pen.0 += match ch {
                            b'^' => 12,
                            b'\'' => 3,
                            _ => 8,
                        };
                    }
                } else {
                    self.win.flags |= 0x8000;
                    self.win.fd = 1;
                    return;
                }
            }
            p.at += if env.strings.byte(p, 0) < 0x80 { 1 } else { 2 };
            if self.win.flags & 0x1000 != 0 && env.strings.byte(p, 0) == 0 {
                self.win.flags &= 0xEFFF;
                p = resume;
            }
            if 1 >= self.units as i32 && self.page_start == 1 {
                self.units = self.units.wrapping_add(1);
            }
            i += 1;
        }
        text = Some(p);
        // parsed:
        if !jumped_to_after && self.state == 0 {
            self.units = self.units.wrapping_add(1);
            return;
        }
        // after_text:
        if self.page_start == 1 {
            self.page_start = 0;
        }
        if self.units == 100 {
            self.units = 101;
            return;
        }
        if self.state == 6 && env.confirm {
            self.state = 0;
            self.page_start = 1;
            env.sfx.push(0xFFFF); // Sfx_QueueRequest(2, 0x40, 0x40): the page-advance blip.
            let mut p = text.unwrap_or(p);
            if env.strings.byte(p, 0) == 0 {
                match self.arg {
                    2 => {
                        self.state = 3;
                        *env.signal = 3;
                    }
                    3 => {
                        self.state = 1;
                        *env.signal = 1;
                        return;
                    }
                    _ => {
                        self.state = 2;
                        *env.text = Some(p);
                        self.win.fd = 2;
                        self.page_start = 1;
                        return;
                    }
                }
            } else {
                p.at += 1;
                *env.text = Some(p);
            }
            self.units = 0;
        }
    }

    /// `Dialogue_MessageBubbleTask`: fixed at (0x33, 0x9B), slides in on (re)open.
    fn step_bubble(&mut self, env: &TextEnv) {
        let Some(b) = self.bubble.as_mut() else { return };
        if env.speaker.is_none() {
            self.bubble = None;
            self.win.flags &= !0x80;
            return;
        }
        let tick = b.tick;
        b.tick += 1;
        if tick == 0 || self.win.count <= 0 {
            return;
        }
        let mut offset = 0i16;
        match b.state {
            0 | 1 => {
                b.state = 1;
                if tick < 6 {
                    offset = 0x18 - 4 * tick as i16;
                }
                if self.win.fd == 2 {
                    b.tick = 1;
                    b.state = 2;
                }
            }
            2 => {
                if self.win.fd == 1 {
                    b.state = 1;
                    b.tick = 1;
                }
                return;
            }
            _ => {}
        }
        self.out.bubble = Some((0x33 + offset, 0x9B + offset));
    }

    /// `Dialogue_BounceCursorTask`.
    fn step_cursor(&mut self) {
        let Some(c) = self.cursor.as_mut() else { return };
        let mut offset = (c.time + ((c.time as u32 >> 31) as i32)) >> 1;
        if offset >= 9 {
            offset -= 8;
            c.flip = !c.flip;
            c.time -= 0x10;
        }
        if c.flip {
            offset = 8 - offset;
        }
        self.out.cursor = Some((self.win.x + 0xCC, self.win.y + 0x20 + offset as i16));
        c.time += if c.tick == 0 { 1 } else { 2 };
        c.tick += 1;
        if self.page_start == 1 {
            self.cursor = None;
        }
    }
}
