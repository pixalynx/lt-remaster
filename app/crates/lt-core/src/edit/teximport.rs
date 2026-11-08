//! PNG ↔ TIM pixels (screen 07 §5, research 06 phase 1): a texture painted
//! outside the app comes back into the TIM it was exported from.
//!
//! Every TIM is uploaded to the VRAM rectangle it names and the models
//! address it with absolute UVs, CLUT positions and texture pages, so an
//! import keeps the rectangle, the depth and the CLUT block: the PNG must be
//! exactly the TIM's size, and only the pixel block and (optionally) one
//! palette change, in place. Nothing in the file moves.
//!
//! - **Export** writes an indexed PNG (4 or 8 bpp) through one palette, with
//!   entry 0x0000 transparent, or an RGBA PNG for 16 bpp.
//! - **Import, keep the palette**: every pixel maps to the nearest colour of
//!   that palette (OKLab), transparent pixels to an entry that is 0x0000. An
//!   indexed PNG whose palette is the TIM's (to the 5 bits the PS1 keeps)
//!   keeps its indices exactly, so export → edit → import is lossless.
//! - **Import, new palette**: the image's colours are reduced to the palette
//!   size (median cut, then k-means in OKLab); index 0 stays transparent when
//!   the image has transparent pixels. The STP bit of each entry is kept.
//!   Other palettes of the TIM keep their colours over the new indices.
//!
//! Opaque black is written 0x8000, because 0x0000 draws transparent.

use std::io::Cursor;

use serde::{Deserialize, Serialize};

use crate::tim::{bgr555_to_rgba, Tim};

use super::{EditError, Result};

/// How an import treats the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// Map to the colours the palette has.
    Keep,
    /// Build a new palette from the image (replaces the palette's colours).
    NewPalette,
}

/// What an import did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub width: u32,
    pub height: u32,
    pub bpp: u8,
    /// The PNG's indices were used as they are (its palette is the TIM's).
    pub lossless: bool,
    /// Palette entries the pixels use.
    pub colours_used: u32,
    /// Entries of the palette that changed (new-palette mode).
    pub palette_changed: u32,
    /// Mean colour error per opaque pixel, in OKLab ×100 (0 = exact).
    pub mean_error: f32,
    pub transparent_pixels: u32,
    pub pixels_changed: u32,
    /// Other palettes of the TIM that now draw the new indices with their old colours.
    pub other_palettes: u32,
}

fn bad(code: &'static str, m: impl Into<String>) -> EditError {
    EditError::new(code, m)
}

fn tim_at(file: &[u8], at: usize) -> Result<Tim<'_>> {
    file.get(at..).and_then(Tim::parse).ok_or_else(|| bad("NOT_A_TIM", format!("No TIM at offset {at:#x}.")))
}

/// File offsets of the TIM's CLUT data and pixel data.
fn layout(file: &[u8], at: usize) -> Result<(Option<usize>, usize)> {
    let t = tim_at(file, at)?;
    let le32 = |o: usize| u32::from_le_bytes(file[o..o + 4].try_into().expect("4 bytes")) as usize;
    let mut p = at + 8;
    let clut = if t.info.clut.is_some() {
        let c = p + 12;
        p += le32(p);
        Some(c)
    } else {
        None
    };
    Ok((clut, p + 12))
}

/* ───────────────────────── Colour ───────────────────────── */

/// sRGB 8-bit → OKLab.
fn oklab([r, g, b]: [u8; 3]) -> [f32; 3] {
    let lin = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(r), lin(g), lin(b));
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// RGB8 → BGR555 (no STP), rounding.
fn to_555([r, g, b]: [u8; 3]) -> u16 {
    let q = |c: u8| ((c as u16 * 31 + 127) / 255) & 31;
    q(r) | q(g) << 5 | q(b) << 10
}

fn rgb_of(c: u16) -> [u8; 3] {
    let [r, g, b, _] = bgr555_to_rgba(c | 0x8000);
    [r, g, b]
}

/// An opaque colour as a CLUT word: never 0x0000.
fn opaque(w: u16) -> u16 {
    if w & 0x7FFF == 0 {
        0x8000
    } else {
        w
    }
}

/* ───────────────────────── PNG ───────────────────────── */

/// A decoded PNG: RGBA8 pixels, and its palette when it is indexed.
pub struct Png {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Indices and palette (RGB8) of an indexed PNG of depth ≤ 8.
    pub indexed: Option<(Vec<u8>, Vec<[u8; 3]>)>,
}

pub fn decode_png(bytes: &[u8]) -> Result<Png> {
    let err = |e: png::DecodingError| bad("PNG_BAD", format!("The PNG can't be read: {e}."));
    // Indices first, if it is indexed.
    let indexed = {
        let mut d = png::Decoder::new(Cursor::new(bytes));
        d.set_transformations(png::Transformations::IDENTITY);
        let mut r = d.read_info().map_err(err)?;
        let info = r.info().clone();
        if info.color_type == png::ColorType::Indexed && (info.bit_depth as u8) <= 8 {
            let mut buf = vec![0; r.output_buffer_size().unwrap_or(0)];
            let f = r.next_frame(&mut buf).map_err(err)?;
            let bits = info.bit_depth as u8 as usize;
            let (w, h) = (info.width as usize, info.height as usize);
            let mut idx = Vec::with_capacity(w * h);
            for y in 0..h {
                let line = &buf[y * f.line_size..(y + 1) * f.line_size];
                for x in 0..w {
                    let bit = x * bits;
                    let byte = line[bit / 8];
                    let shift = 8 - bits - (bit % 8);
                    idx.push((byte >> shift) & ((1u16 << bits) - 1) as u8);
                }
            }
            let pal = info.palette.as_ref().map(|p| p.chunks(3).map(|c| [c[0], c[1], c[2]]).collect()).unwrap_or_default();
            Some((idx, pal))
        } else {
            None
        }
    };
    let mut d = png::Decoder::new(Cursor::new(bytes));
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA);
    let mut r = d.read_info().map_err(err)?;
    let mut buf = vec![0; r.output_buffer_size().unwrap_or(0)];
    let f = r.next_frame(&mut buf).map_err(err)?;
    let (w, h) = (f.width, f.height);
    let ch = match f.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        _ => 1,
    };
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        let line = &buf[y * f.line_size..y * f.line_size + w as usize * ch];
        for px in line.chunks(ch) {
            rgba.extend_from_slice(&match ch {
                4 => [px[0], px[1], px[2], px[3]],
                3 => [px[0], px[1], px[2], 255],
                2 => [px[0], px[0], px[0], px[1]],
                _ => [px[0], px[0], px[0], 255],
            });
        }
    }
    Ok(Png { width: w, height: h, rgba, indexed })
}

/* ───────────────────────── Export ───────────────────────── */

/// The TIM at `at` as a PNG, through palette (`row`, `col`).
pub fn export_png(file: &[u8], at: usize, row: usize, col: usize) -> Result<Vec<u8>> {
    let t = tim_at(file, at)?;
    let (w, h) = (t.info.width, t.info.height);
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    let (_, pix_at) = layout(file, at)?;
    let n = (w * h) as usize;
    let data: Vec<u8> = match t.info.bpp {
        4 | 8 => {
            let pal = t.palette(row, col);
            if pal.is_empty() {
                return Err(bad("TIM_CLUT_RANGE", format!("The TIM has no palette {row}/{col}.")));
            }
            let plte: Vec<u8> = pal.iter().flat_map(|&c| rgb_of(c)).collect();
            let trns: Vec<u8> = pal.iter().map(|&c| if c == 0 { 0 } else { 255 }).collect();
            enc.set_color(png::ColorType::Indexed);
            enc.set_depth(if t.info.bpp == 4 { png::BitDepth::Four } else { png::BitDepth::Eight });
            enc.set_palette(plte);
            enc.set_trns(trns);
            let raw = &file[pix_at..pix_at + if t.info.bpp == 4 { n / 2 } else { n }];
            if t.info.bpp == 4 {
                // TIM: low nibble first; PNG: high nibble first.
                raw.iter().map(|b| (b & 0x0F) << 4 | b >> 4).collect()
            } else {
                raw.to_vec()
            }
        }
        _ => {
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            t.rgba(0, 0)
        }
    };
    {
        let mut wr = enc.write_header().map_err(|e| bad("PNG_WRITE", e.to_string()))?;
        wr.write_image_data(&data).map_err(|e| bad("PNG_WRITE", e.to_string()))?;
    }
    Ok(out)
}

/* ───────────────────────── Quantising ───────────────────────── */

/// Up to `k` colours for `px` (RGB8, with repeats): median cut on the
/// widest channel, then a few k-means passes in OKLab.
fn quantise(px: &[[u8; 3]], k: usize) -> Vec<[u8; 3]> {
    let mut uniq: Vec<[u8; 3]> = px.to_vec();
    uniq.sort_unstable();
    uniq.dedup();
    if uniq.len() <= k {
        return uniq;
    }
    // Median cut over the pixels (weighted by count).
    let mut boxes: Vec<Vec<[u8; 3]>> = vec![px.to_vec()];
    while boxes.len() < k {
        let (bi, axis) = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .map(|(i, b)| {
                let range = |c: usize| b.iter().map(|p| p[c]).max().unwrap_or(0) - b.iter().map(|p| p[c]).min().unwrap_or(0);
                let (axis, r) = (0..3).map(|c| (c, range(c))).max_by_key(|&(_, r)| r).expect("3 axes");
                (i, axis, r as usize * b.len())
            })
            .max_by_key(|&(_, _, score)| score)
            .map(|(i, a, _)| (i, a))
            .unwrap_or((usize::MAX, 0));
        if bi == usize::MAX {
            break;
        }
        let mut b = boxes.swap_remove(bi);
        b.sort_unstable_by_key(|p| p[axis]);
        let hi = b.split_off(b.len() / 2);
        boxes.push(b);
        boxes.push(hi);
    }
    let mean = |b: &[[u8; 3]]| {
        let n = b.len().max(1) as u32;
        let s = b.iter().fold([0u32; 3], |a, p| [a[0] + p[0] as u32, a[1] + p[1] as u32, a[2] + p[2] as u32]);
        [(s[0] / n) as u8, (s[1] / n) as u8, (s[2] / n) as u8]
    };
    let mut centres: Vec<[u8; 3]> = boxes.iter().filter(|b| !b.is_empty()).map(|b| mean(b)).collect();
    // k-means refinement in OKLab over the distinct colours (weighted).
    let mut counts: std::collections::HashMap<[u8; 3], u32> = std::collections::HashMap::new();
    for p in px {
        *counts.entry(*p).or_default() += 1;
    }
    let colours: Vec<([u8; 3], [f32; 3], u32)> = counts.iter().map(|(c, &n)| (*c, oklab(*c), n)).collect();
    for _ in 0..6 {
        let labs: Vec<[f32; 3]> = centres.iter().map(|c| oklab(*c)).collect();
        let mut sums = vec![[0f64; 4]; centres.len()];
        for (c, lab, n) in &colours {
            let i = (0..labs.len()).min_by(|&a, &b| dist(*lab, labs[a]).total_cmp(&dist(*lab, labs[b]))).expect("centres");
            let s = &mut sums[i];
            s[0] += c[0] as f64 * *n as f64;
            s[1] += c[1] as f64 * *n as f64;
            s[2] += c[2] as f64 * *n as f64;
            s[3] += *n as f64;
        }
        for (c, s) in centres.iter_mut().zip(&sums) {
            if s[3] > 0.0 {
                *c = [(s[0] / s[3]).round() as u8, (s[1] / s[3]).round() as u8, (s[2] / s[3]).round() as u8];
            }
        }
    }
    centres
}

/* ───────────────────────── Import ───────────────────────── */

/// `file` with the PNG's pixels in the TIM at `at`, mapped through palette
/// (`row`, `col`) as `mode` says.
pub fn import_png(file: &[u8], at: usize, png: &[u8], row: usize, col: usize, mode: Mode) -> Result<(Vec<u8>, ImportReport)> {
    let t = tim_at(file, at)?;
    let img = decode_png(png)?;
    let (w, h) = (t.info.width, t.info.height);
    if (img.width, img.height) != (w, h) {
        return Err(bad(
            "TEX_SIZE",
            format!("The PNG is {}×{}; this texture is {w}×{h}. Its VRAM rectangle and the models' UVs are fixed, so the size must match.", img.width, img.height),
        ));
    }
    let n = (w * h) as usize;
    let (clut_at, pix_at) = layout(file, at)?;
    let mut out = file.to_vec();
    let px = |i: usize| -> ([u8; 3], u8) {
        let p = &img.rgba[i * 4..i * 4 + 4];
        ([p[0], p[1], p[2]], p[3])
    };
    let transparent_pixels = (0..n).filter(|&i| px(i).1 < 128).count() as u32;
    let mut report = ImportReport {
        width: w,
        height: h,
        bpp: t.info.bpp,
        lossless: false,
        colours_used: 0,
        palette_changed: 0,
        mean_error: 0.0,
        transparent_pixels,
        pixels_changed: 0,
        other_palettes: 0,
    };

    if t.info.bpp == 16 {
        let mut err = 0f32;
        let mut opaque_n = 0u32;
        for i in 0..n {
            let (c, a) = px(i);
            let word = if a < 128 { 0 } else { opaque(to_555(c)) };
            if a >= 128 {
                err += dist(oklab(c), oklab(rgb_of(word))).sqrt();
                opaque_n += 1;
            }
            let o = pix_at + i * 2;
            if out[o..o + 2] != word.to_le_bytes() {
                report.pixels_changed += 1;
            }
            out[o..o + 2].copy_from_slice(&word.to_le_bytes());
        }
        report.mean_error = err / opaque_n.max(1) as f32 * 100.0;
        return Ok((out, report));
    }

    let entries = if t.info.bpp == 8 { 256 } else { 16 };
    let old = t.palette(row, col);
    if old.len() != entries {
        return Err(bad("TIM_CLUT_RANGE", format!("The TIM has no palette {row}/{col}.")));
    }
    let mut pal = old.clone();
    // The indices for every pixel.
    let indices: Vec<u8> = match (&img.indexed, mode) {
        (Some((idx, ipal)), _) if ipal.len() <= entries && ipal.iter().enumerate().all(|(i, c)| old[i] != 0 && to_555(*c) == old[i] & 0x7FFF || old[i] == 0) && idx.iter().all(|&v| (v as usize) < entries) => {
            report.lossless = true;
            idx.clone()
        }
        (_, Mode::Keep) => {
            let transparent = old.iter().position(|&c| c == 0);
            if transparent_pixels > 0 && transparent.is_none() {
                return Err(bad("TEX_NO_TRANSPARENT", "The image has transparent pixels but this palette has no transparent (0x0000) entry. Use a new palette, or make the pixels opaque."));
            }
            let choices: Vec<(usize, [f32; 3])> = old.iter().enumerate().filter(|(_, &c)| c != 0).map(|(i, &c)| (i, oklab(rgb_of(c)))).collect();
            if choices.is_empty() {
                return Err(bad("TEX_EMPTY_PALETTE", "Every entry of this palette is transparent."));
            }
            let mut cache: std::collections::HashMap<[u8; 3], u8> = std::collections::HashMap::new();
            (0..n)
                .map(|i| {
                    let (c, a) = px(i);
                    if a < 128 {
                        return transparent.expect("checked") as u8;
                    }
                    *cache.entry(c).or_insert_with(|| {
                        let lab = oklab(c);
                        choices.iter().min_by(|a, b| dist(lab, a.1).total_cmp(&dist(lab, b.1))).expect("choices").0 as u8
                    })
                })
                .collect()
        }
        (_, Mode::NewPalette) => {
            let reserve = usize::from(transparent_pixels > 0);
            let opaque_px: Vec<[u8; 3]> = (0..n).filter(|&i| px(i).1 >= 128).map(|i| px(i).0).collect();
            let colours = quantise(&opaque_px, entries - reserve);
            let base = reserve;
            if reserve == 1 {
                pal[0] = 0;
            }
            for (k, c) in colours.iter().enumerate() {
                let i = base + k;
                pal[i] = opaque(to_555(*c) | (old[i] & 0x8000));
            }
            // Unused entries keep their old colour (still never transparent unless they were).
            let labs: Vec<(usize, [f32; 3])> = (base..base + colours.len()).map(|i| (i, oklab(rgb_of(pal[i])))).collect();
            let mut cache: std::collections::HashMap<[u8; 3], u8> = std::collections::HashMap::new();
            (0..n)
                .map(|i| {
                    let (c, a) = px(i);
                    if a < 128 {
                        return 0;
                    }
                    *cache.entry(c).or_insert_with(|| {
                        let lab = oklab(c);
                        labs.iter().min_by(|a, b| dist(lab, a.1).total_cmp(&dist(lab, b.1))).expect("labs").0 as u8
                    })
                })
                .collect()
        }
    };

    // Write the palette and the pixels.
    if let (Some(c), Some(rect)) = (clut_at, t.info.clut) {
        let start = c + (row * rect.w as usize + col * entries) * 2;
        for (i, &word) in pal.iter().enumerate() {
            if word != old[i] {
                report.palette_changed += 1;
            }
            out[start + i * 2..start + i * 2 + 2].copy_from_slice(&word.to_le_bytes());
        }
    }
    let mut used = vec![false; entries];
    let mut err = 0f32;
    let mut opaque_n = 0u32;
    for (i, &v) in indices.iter().enumerate() {
        used[v as usize] = true;
        let (c, a) = px(i);
        if a >= 128 && pal[v as usize] != 0 {
            err += dist(oklab(c), oklab(rgb_of(pal[v as usize]))).sqrt();
            opaque_n += 1;
        }
    }
    report.colours_used = used.iter().filter(|&&u| u).count() as u32;
    report.mean_error = err / opaque_n.max(1) as f32 * 100.0;
    let before = out[pix_at..pix_at + if entries == 16 { n / 2 } else { n }].to_vec();
    if entries == 16 {
        for (k, pair) in indices.chunks(2).enumerate() {
            out[pix_at + k] = (pair[0] & 0x0F) | (pair.get(1).copied().unwrap_or(0) & 0x0F) << 4;
        }
    } else {
        out[pix_at..pix_at + n].copy_from_slice(&indices);
    }
    report.pixels_changed = if entries == 16 {
        (0..n).filter(|&i| (before[i / 2] >> ((i % 2) * 4)) & 0x0F != indices[i]).count() as u32
    } else {
        (0..n).filter(|&i| before[i] != indices[i]).count() as u32
    };
    let rows = t.info.clut.map_or(0, |c| c.h as usize);
    if report.pixels_changed > 0 {
        report.other_palettes = (rows * t.palettes_per_row()).saturating_sub(1) as u32;
    }
    Ok((out, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4bpp 8×2 TIM with one 16-entry palette: entry 0 transparent, 1 red, 2 green, 3 blue.
    fn tim4() -> Vec<u8> {
        let mut b = Vec::new();
        let w32 = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        let w16 = |b: &mut Vec<u8>, v: u16| b.extend_from_slice(&v.to_le_bytes());
        w32(&mut b, 0x10);
        w32(&mut b, 0x08);
        w32(&mut b, 12 + 32);
        for v in [0u16, 480, 16, 1] {
            w16(&mut b, v);
        }
        let mut pal = [0x8000u16; 16];
        pal[0] = 0;
        pal[1] = 0x001F;
        pal[2] = 0x03E0;
        pal[3] = 0x7C00;
        pal.iter().for_each(|&c| w16(&mut b, c));
        w32(&mut b, 12 + 8);
        for v in [640u16, 0, 2, 2] {
            w16(&mut b, v);
        }
        b.extend_from_slice(&[0x21, 0x03, 0x10, 0x32, 0x00, 0x11, 0x22, 0x33]);
        b
    }

    #[test]
    fn export_then_import_is_lossless() {
        let t = tim4();
        let png = export_png(&t, 0, 0, 0).unwrap();
        let (out, rep) = import_png(&t, 0, &png, 0, 0, Mode::Keep).unwrap();
        assert!(rep.lossless);
        assert_eq!(out, t);
        assert_eq!(rep.pixels_changed, 0);
        // The RGBA path (no indexed palette) also lands on the same indices.
        let decoded = decode_png(&png).unwrap();
        let mut rgba_png = Vec::new();
        {
            let mut e = png::Encoder::new(&mut rgba_png, 8, 2);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&decoded.rgba).unwrap();
        }
        let (out2, rep2) = import_png(&t, 0, &rgba_png, 0, 0, Mode::Keep).unwrap();
        assert!(!rep2.lossless);
        assert_eq!(out2, t);
        assert_eq!(rep2.transparent_pixels, 4);
    }

    #[test]
    fn a_new_palette_holds_the_image_colours() {
        let t = tim4();
        // A 8×2 image: 3 opaque colours and transparent pixels.
        let colours = [[0u8, 0, 0, 0], [200, 100, 50, 255], [10, 20, 30, 255], [255, 255, 255, 255]];
        let mut rgba = Vec::new();
        for i in 0..16 {
            rgba.extend_from_slice(&colours[i % 4]);
        }
        let mut p = Vec::new();
        {
            let mut e = png::Encoder::new(&mut p, 8, 2);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&rgba).unwrap();
        }
        let (out, rep) = import_png(&t, 0, &p, 0, 0, Mode::NewPalette).unwrap();
        assert_eq!(rep.colours_used, 4);
        assert!(rep.mean_error < 2.0, "{}", rep.mean_error);
        let back = Tim::parse(&out).unwrap();
        let decoded = back.rgba(0, 0);
        for i in 0..16 {
            let want = colours[i % 4];
            let got = &decoded[i * 4..i * 4 + 4];
            if want[3] == 0 {
                assert_eq!(got[3], 0);
            } else {
                for c in 0..3 {
                    assert!((got[c] as i32 - want[c] as i32).abs() <= 8, "pixel {i}: {got:?} vs {want:?}");
                }
            }
        }
        assert_eq!(out.len(), t.len(), "in place");
        // Wrong size is refused.
        let mut small = Vec::new();
        {
            let mut e = png::Encoder::new(&mut small, 4, 2);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&[0u8; 32]).unwrap();
        }
        assert_eq!(import_png(&t, 0, &small, 0, 0, Mode::Keep).unwrap_err().code, "TEX_SIZE");
    }
}
