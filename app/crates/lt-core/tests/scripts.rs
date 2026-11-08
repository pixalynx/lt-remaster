//! The Tier-2 assembler and the script layout against the user's own retail
//! image (screen 11, PRD 11 §5.6 and §8). Opt in with
//! `LT_TEST_DISC=/path/to/Guardian's Crusade.bin cargo test -p lt-core --test scripts`.
//! The image is opened read-only.

use std::collections::BTreeMap;

use lt_core::edit::{script, text as text_edit};
use lt_core::fs::Kind;
use lt_core::npc::NpcPack;
use lt_core::tier2::asm::{self, Limits};
use lt_core::tier2::ast::{self, TextRef};
use lt_core::tier2::dsl::{self, Severity};
use lt_core::tier2::{self, Operand};
use lt_core::{DiscImage, FileTable, PsxExe};

struct Disc {
    image: DiscImage,
    table: FileTable,
}

fn disc() -> Option<Disc> {
    let path = std::env::var_os("LT_TEST_DISC")?;
    let image = DiscImage::open(path).expect("LT_TEST_DISC must be a raw .bin");
    let exe = PsxExe::from_disc(&image).unwrap();
    let table = FileTable::parse(&exe, image.sectors()).unwrap();
    Some(Disc { image, table })
}

impl Disc {
    fn read(&self, id: &str) -> Vec<u8> {
        let e = self.table.by_id(id).unwrap();
        self.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap()
    }

    /// Every distinct pack (npc/17 aliases npc/0).
    fn packs(&self) -> Vec<(String, Vec<u8>, NpcPack)> {
        self.table
            .of_kind(Kind::Npc)
            .filter(|e| e.aliases.is_empty() || e.global < e.aliases[0])
            .map(|e| {
                let b = self.image.read_sectors(e.extent.lba, e.extent.sectors).unwrap();
                let p = NpcPack::parse(&b).unwrap();
                (e.id.clone(), b, p)
            })
            .collect()
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// PRD 11 §5.6: every program (actor roots and GOTO targets) of every pack
/// goes bytes → tree → DSL → tree → bytes unchanged, prints stably, and
/// passes validation without errors.
#[test]
fn every_program_round_trips_through_the_dsl() {
    let Some(disc) = disc() else { return };
    let (mut programs, mut instrs, mut gotos) = (0usize, 0usize, 0usize);
    let mut warnings: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (id, b, pack) in disc.packs() {
        let d = pack.d_block(&b);
        let pool: std::collections::BTreeSet<u16> = pack.pool(&b).iter().map(|s| s.offset as u16).collect();
        let progs = pack.programs(&b);
        let starts: std::collections::BTreeSet<u16> = progs.iter().filter_map(|(_, _, r)| r.as_ref().ok()).flatten().map(|i| i.at as u16).collect();
        let actor_entries: std::collections::BTreeSet<u32> = pack.actors.iter().map(|a| a.script).collect();
        for (_, entry, res) in progs {
            let code = res.unwrap_or_else(|e| panic!("{id} D+0x{entry:04X}: {e:?}"));
            let bytes: Vec<u8> = code.iter().flat_map(|i| i.bytes.iter().copied()).collect();
            let tree = ast::lift(&code, &|t| TextRef::Pool(t));
            let text = dsl::print(&tree, None, &|_| None);
            let (parsed, diags) = dsl::parse(&text);
            assert!(diags.is_empty(), "{id} D+0x{entry:04X}: {diags:?}\n{text}");
            assert_eq!(dsl::print(&parsed, None, &|_| None), text, "{id} D+0x{entry:04X} prints stably");
            let identity = |t: &TextRef| match t {
                TextRef::Pool(o) => Ok(*o),
                TextRef::New(_) => Err("new".to_string()),
            };
            let again = asm::assemble(&parsed, entry, &identity).unwrap_or_else(|e| panic!("{id} D+0x{entry:04X}: {e:?}\n{text}"));
            assert_eq!(again, bytes, "{id} D+0x{entry:04X} re-assembles to the retail bytes\n{text}");
            let lim = Limits { actors: pack.actors.len() as u32, pool: &|o| pool.contains(&o), code: &|o| starts.contains(&o), d_span: pack.d_span, flags: 512, areas: &[] };
            for diag in asm::validate(&parsed, &lim) {
                assert_ne!(diag.severity, Severity::Error, "{id} D+0x{entry:04X} retail fails validation: {diag:?}\n{text}");
                *warnings.entry(diag.code).or_default() += 1;
            }
            programs += 1;
            instrs += code.len();
            if !actor_entries.contains(&entry) {
                gotos += 1;
            }
            let _ = d;
        }
    }
    eprintln!("{programs} programs ({gotos} reached only by GOTO), {instrs} instructions; retail warnings {warnings:?}");
    assert!(programs > 1500, "{programs}");
}

/// Saving each actor's own DSL back changes nothing: the plan says
/// "identical" and the laid-out pack is the retail pack.
#[test]
fn saving_retail_programs_is_a_no_op() {
    let Some(disc) = disc() else { return };
    for id in ["npc/7", "npc/13", "npc/40"] {
        let b = disc.read(id);
        let pack = NpcPack::parse(&b).unwrap();
        for slot in 0..pack.actors.len() as u32 {
            if pack.actors[slot as usize].script == 0 {
                continue;
            }
            let src = script::source(&b, &b, slot, id).unwrap();
            assert!(!src.edited);
            let chk = script::check(&b, &b, slot, Some(&src.dsl), &[]).unwrap();
            let errors: Vec<_> = chk.diagnostics.iter().filter(|d| d.severity == Severity::Error).collect();
            assert!(errors.is_empty(), "{id} actor {slot}: {errors:?}\n{}", src.dsl);
            let plan = chk.plan.unwrap();
            assert!(plan.identical, "{id} actor {slot} is identical\n{}", src.dsl);
            assert_eq!(chk.bytes.unwrap(), b, "{id} actor {slot}: layout of no edits is retail");
        }
    }
}

/// PRD 11 §8: inserting `give 5` before the wake-up scene's WARP and saving
/// appends the program (84 + 3 bytes, padded to 88) at the end of the D-span,
/// repoints record 3 only, shifts the models, and reads back.
#[test]
fn the_wake_up_scene_takes_an_extra_item() {
    let Some(disc) = disc() else { return };
    let retail = disc.read("npc/7");
    let r = NpcPack::parse(&retail).unwrap();
    assert_eq!(r.actors[3].script, 0x2024);
    let src = script::source(&retail, &retail, 3, "npc/7").unwrap();
    assert_eq!((src.ops, src.bytes), (30, 84));
    assert!(src.dsl.contains("warp facing:0 chunk/78 x:-32 z:-880 flags:0x0201"), "{}", src.dsl);
    let edited = src.dsl.replace("  warp facing:0", "  give 5\n  warp facing:0");

    let chk = script::check(&retail, &retail, 3, Some(&edited), &[]).unwrap();
    assert!(chk.diagnostics.iter().all(|d| d.severity != Severity::Error), "{:?}", chk.diagnostics);
    let plan = chk.plan.clone().unwrap();
    assert!(!plan.identical);
    assert_eq!((plan.ops, plan.bytes), (31, 87));
    assert_eq!(plan.at, Some(r.d_span), "appended at the retail end of the D-span");
    assert_eq!(plan.slots, vec![3]);
    assert_eq!(plan.report.appended, 88);
    assert_eq!(plan.report.script_bytes, 88);
    let out = chk.bytes.unwrap();

    // Only record 3's `a`, the model table and the inserted area differ (models shift by 88).
    let o = NpcPack::parse(&out).unwrap();
    assert_eq!(o.actors[3].script, r.d_span);
    for (k, (ra, oa)) in r.actors.iter().zip(&o.actors).enumerate() {
        if k != 3 {
            assert_eq!(ra.script, oa.script, "actor {k}");
        }
    }
    let first_model = r.d + r.d_span;
    for (rm, om) in r.models.iter().zip(&o.models) {
        assert_eq!(*om, if *rm >= first_model { rm + 88 } else { *rm });
    }
    let fm = first_model as usize;
    assert_eq!(out[fm + 88..fm + 88 + 4096], retail[fm..fm + 4096], "models moved as a block");
    let rec3 = r.d as usize + 4 + 16 * 3;
    let mut masked = out[..fm].to_vec();
    masked[rec3..rec3 + 4].copy_from_slice(&retail[rec3..rec3 + 4]);
    let table = 8..8 + 4 * r.models.len();
    masked[table.clone()].copy_from_slice(&retail[table]);
    let diff: Vec<usize> = (0..fm).filter(|&k| masked[k] != retail[k]).collect();
    assert!(diff.is_empty(), "nothing else below the area changed: {} bytes at {:X?}", diff.len(), &diff[..diff.len().min(16)]);

    // The program reads back with the GIVE before the WARP, and the DSL comes back the same.
    let code = tier2::disassemble(o.d_block(&out), r.d_span).unwrap();
    let ops: Vec<u8> = code.iter().map(|i| i.op).collect();
    let warp = ops.iter().position(|&op| op == 0x19).unwrap();
    assert_eq!((ops[warp - 1], code[warp - 1].operands[0]), (0x16, Operand::Item(5)));
    let back = script::source(&retail, &out, 3, "npc/7").unwrap();
    assert!(back.edited);
    assert_eq!(body(&back.dsl), body(&edited), "the edit reads back");
    assert_eq!(script::edited(&retail, &out), vec![(0x2024, vec![3])]);

    // A text save keeps the program; saving the retail DSL again reverts it.
    let (kept, _) = text_edit::apply(&retail, &out, &BTreeMap::new()).unwrap();
    assert_eq!(kept, out, "a text save with no lines keeps the program in place");
    let revert = script::check(&retail, &out, 3, Some(&src.dsl), &[]).unwrap();
    assert!(revert.plan.as_ref().unwrap().identical);
    assert_eq!(revert.bytes.unwrap(), retail, "back to retail");
    let none = script::check(&retail, &out, 3, None, &[]).unwrap();
    assert_eq!(none.bytes.unwrap(), retail);
}

/// A WARP into a new area saves once the project has that area, through the
/// DSL and the Structured tree alike; the operand is the plain id.
#[test]
fn a_warp_into_a_new_area_saves_when_the_project_has_it() {
    use script::{Arg, Node};
    fn retarget(nodes: &mut [Node], chunk: i64) -> bool {
        nodes.iter_mut().any(|n| match n {
            Node::Op { op: 0x19, args, .. } => {
                (args[1], args[2], args[3]) = (Arg::Num { value: chunk }, Arg::Num { value: 0 }, Arg::Num { value: -512 });
                true
            }
            Node::If { then, els, .. } => retarget(then, chunk) || retarget(els, chunk),
            _ => false,
        })
    }
    let Some(disc) = disc() else { return };
    let retail = disc.read("npc/7");
    let errors = |c: &script::Check| c.diagnostics.iter().filter(|d| d.severity == Severity::Error).map(|d| d.code).collect::<Vec<_>>();
    let src = script::source(&retail, &retail, 3, "npc/7").unwrap();
    let edited = src.dsl.replace("warp facing:0 chunk/78 x:-32 z:-880", "warp facing:0 chunk/1328 x:0 z:-512");
    assert_ne!(edited, src.dsl);

    let refused = script::check(&retail, &retail, 3, Some(&edited), &[]).unwrap();
    assert_eq!(errors(&refused), vec!["CHUNK_RANGE"]);
    assert!(refused.bytes.is_none());
    assert_eq!(errors(&script::check(&retail, &retail, 3, Some(&edited), &[1329]).unwrap()), vec!["CHUNK_RANGE"], "another new area doesn't count");

    let chk = script::check(&retail, &retail, 3, Some(&edited), &[1328]).unwrap();
    assert!(errors(&chk).is_empty(), "{:?}", chk.diagnostics);
    let out = chk.bytes.unwrap();
    let o = NpcPack::parse(&out).unwrap();
    let code = tier2::disassemble(o.d_block(&out), o.actors[3].script).unwrap();
    let warp = code.iter().find(|i| i.op == 0x19).unwrap();
    assert_eq!(warp.operands[1], Operand::Chunk(1328));
    assert_eq!(warp.bytes[3..5], 1328u16.to_le_bytes(), "the plain id; TriggerDialogueEvent adds 0x8000");
    let back = script::source(&retail, &out, 3, "npc/7").unwrap();
    assert!(back.dsl.contains("warp facing:0 chunk/1328 x:0 z:-512 flags:0x0201"), "{}", back.dsl);
    assert_eq!(body(&back.dsl), body(&edited));

    let mut tree = src.tree.clone();
    assert!(retarget(&mut tree, 1328));
    assert_eq!(errors(&script::check_tree(&retail, &retail, 3, Some(&tree), &[]).unwrap()), vec!["CHUNK_RANGE"]);
    let by_tree = script::check_tree(&retail, &retail, 3, Some(&tree), &[1328]).unwrap();
    assert_eq!(by_tree.bytes.unwrap(), out, "same layout as the DSL");
}

/// The Structured view's tree: every node of a retail program carries its
/// address, saving the tree unchanged is a no-op, and an insert through the
/// tree lays out the same bytes as the same insert through the DSL.
#[test]
fn the_structured_tree_round_trips() {
    use script::{Arg, Node};
    fn walk(nodes: &[Node], ats: &mut Vec<u32>) {
        for n in nodes {
            match n {
                Node::Op { at, .. } => ats.push(at.expect("an address")),
                Node::If { at, then, els, else_at, .. } => {
                    ats.push(at.expect("an address"));
                    walk(then, ats);
                    ats.push(else_at.expect("an ELSE address"));
                    walk(els, ats);
                }
                Node::Label { .. } => {}
            }
        }
    }
    let Some(disc) = disc() else { return };
    for id in ["npc/7", "npc/13", "npc/40"] {
        let b = disc.read(id);
        let pack = NpcPack::parse(&b).unwrap();
        for slot in 0..pack.actors.len() as u32 {
            if pack.actors[slot as usize].script == 0 {
                continue;
            }
            let src = script::source(&b, &b, slot, id).unwrap();
            let code = tier2::disassemble(pack.d_block(&b), src.at).unwrap();
            let mut ats = Vec::new();
            walk(&src.tree, &mut ats);
            let shown: Vec<u32> = code.iter().filter(|i| i.op != 0x0D && i.op != 0x0F).map(|i| i.at).collect();
            if !code.iter().any(|i| i.op == 0x0D || i.op == 0x0F) || ats.len() == shown.len() {
                assert_eq!(ats, shown, "{id} actor {slot}: one address per Structured row");
            }
            let json = serde_json::to_string(&src.tree).unwrap();
            let back: Vec<Node> = serde_json::from_str(&json).unwrap();
            assert_eq!(back, src.tree, "{id} actor {slot}: JSON round trip");
            let chk = script::check_tree(&b, &b, slot, Some(&back), &[]).unwrap();
            assert!(chk.plan.as_ref().is_some_and(|p| p.identical), "{id} actor {slot}: {:?}", chk.diagnostics);
            assert_eq!(chk.bytes.unwrap(), b, "{id} actor {slot}");
        }
    }

    let retail = disc.read("npc/7");
    let src = script::source(&retail, &retail, 3, "npc/7").unwrap();
    let mut tree = src.tree.clone();
    fn insert_before_warp(nodes: &mut Vec<Node>) -> bool {
        if let Some(k) = nodes.iter().position(|n| matches!(n, Node::Op { op: 0x19, .. })) {
            nodes.insert(k, Node::Op { op: 0x16, args: vec![Arg::Num { value: 5 }], at: None });
            return true;
        }
        nodes.iter_mut().any(|n| match n {
            Node::If { then, els, .. } => insert_before_warp(then) || insert_before_warp(els),
            _ => false,
        })
    }
    assert!(insert_before_warp(&mut tree));
    let edited = src.dsl.replace("  warp facing:0", "  give 5\n  warp facing:0");
    let by_tree = script::check_tree(&retail, &retail, 3, Some(&tree), &[]).unwrap();
    let by_dsl = script::check(&retail, &retail, 3, Some(&edited), &[]).unwrap();
    assert_eq!(by_tree.bytes.unwrap(), by_dsl.bytes.unwrap(), "same layout either way");

    // A new line written as markup comes back as the same markup.
    let line = "{box:0}Hello, $A!{nl}Take this.";
    tree.insert(0, Node::Op { op: 0x01, args: vec![Arg::Text { markup: line.into() }], at: None });
    let chk = script::check_tree(&retail, &retail, 3, Some(&tree), &[]).unwrap();
    assert!(chk.diagnostics.iter().all(|d| d.severity != Severity::Error), "{:?}", chk.diagnostics);
    let out = chk.bytes.unwrap();
    let back = script::source(&retail, &out, 3, "npc/7").unwrap();
    assert_eq!(back.tree.first().map(|n| match n {
        Node::Op { args, .. } => args.clone(),
        _ => Vec::new(),
    }), Some(vec![Arg::Text { markup: line.into() }]));
}

fn body(dsl: &str) -> String {
    dsl.lines().filter(|l| !l.starts_with("//")).map(|l| l.split("  // ").next().unwrap().trim_end()).collect::<Vec<_>>().join("\n")
}

/// Text and script edits compose in either order: a grown line, a program
/// that quotes it and a new string, then another text save.
#[test]
fn text_and_script_edits_compose() {
    let Some(disc) = disc() else { return };
    let retail = disc.read("npc/7");
    let r = NpcPack::parse(&retail).unwrap();
    let d = r.d as usize;
    let line = 0x1BCCu32; // "Good morning!|Wake up, wake up!" in the wake-up scene
    let grown = format!("{}{}", text_edit::edit_set(&retail, &retail).unwrap().get(&line).cloned().unwrap_or_else(|| lt_core::text::markup_at(&retail[d..], line as usize)), " Rise and shine, you sleepy head!");
    let edits = BTreeMap::from([(line, grown.clone())]);

    // 1. Grow the line.
    let (t1, rep1) = text_edit::apply(&retail, &retail, &edits).unwrap();
    assert_eq!(rep1.relocated.len(), 1);
    assert_eq!(rep1.script_bytes, 0);

    // 2. Edit the scene: quote the grown line, add a new one.
    let src = script::source(&retail, &t1, 3, "npc/7").unwrap();
    assert!(src.dsl.contains("@0x1BCC"), "the relocated line is named by its retail offset\n{}", src.dsl);
    let edited = src.dsl.replace("  warp facing:0", "  say \"A brand new line,{nl}just for this test.\"\n  say @0x1BCC\n  warp facing:0");
    let chk = script::check(&retail, &t1, 3, Some(&edited), &[]).unwrap();
    assert!(chk.diagnostics.iter().all(|d| d.severity != Severity::Error), "{:?}", chk.diagnostics);
    let t2 = chk.bytes.unwrap();
    let plan = chk.plan.unwrap();
    assert_eq!(plan.new_strings, 1);
    assert_eq!(text_edit::edit_set(&retail, &t2).unwrap(), edits, "the grown line survives the script save");

    // The program's SAYs point at the relocated copy and the new string.
    let o = NpcPack::parse(&t2).unwrap();
    let od = o.d_block(&t2);
    let moved_to = rep1.relocated[0].at;
    let code = tier2::disassemble(od, o.actors[3].script).unwrap();
    let says: Vec<String> = code
        .iter()
        .filter_map(|i| match i.operands.first() {
            Some(Operand::Text(t)) if i.op == 0x01 => Some(lt_core::text::markup_at(od, *t as usize)),
            _ => None,
        })
        .collect();
    assert!(says.contains(&"A brand new line,{nl}just for this test.".to_string()), "{says:?}");
    assert_eq!(says.iter().filter(|s| **s == grown).count(), 2, "the original SAY and the added one both follow the relocation: {says:?}");
    let _ = moved_to;

    // 3. Another text save (shorter line, back in place): the program stays and re-resolves.
    let short = BTreeMap::from([(line, "Morning!".to_string())]);
    let (t3, rep3) = text_edit::apply(&retail, &t2, &short).unwrap();
    assert_eq!(rep3.relocated.len(), 0);
    let back = script::source(&retail, &t3, 3, "npc/7").unwrap();
    assert!(back.edited);
    assert_eq!(body(&back.dsl), body(&edited));
    let o3 = NpcPack::parse(&t3).unwrap();
    let code3 = tier2::disassemble(o3.d_block(&t3), o3.actors[3].script).unwrap();
    assert!(code3.iter().any(|i| i.op == 0x01 && i.operands[0] == Operand::Text(line as u16)), "the SAY points at the line in place again");
    assert_eq!(le32(&t3, 4), r.d);

    // Recovering and laying out again is stable.
    let scripts = script::recover(&retail, &t3).unwrap();
    let (t4, _, _) = text_edit::layout(&retail, &t3, &lt_core::edit::pack::Edits { text: short.clone(), scripts: scripts.clone(), ..Default::default() }).unwrap();
    assert_eq!(t4, t3);
}

/// The validator stops what would hang or crash the game.
#[test]
fn bad_programs_are_refused() {
    let Some(disc) = disc() else { return };
    let retail = disc.read("npc/7");
    let codes = |src: &str| -> Vec<&'static str> {
        script::check(&retail, &retail, 3, Some(src), &[]).unwrap().diagnostics.into_iter().filter(|d| d.severity == Severity::Error).map(|d| d.code).collect()
    };
    assert_eq!(codes("set 0x200\n"), vec!["FLAG_RANGE"]);
    assert_eq!(codes("objective \"This objective is far too long to fit in the quest log buffer of one hundred and three bytes, really. Truly.\"\n"), vec!["OBJECTIVE_LONG"]);
    assert_eq!(codes("say @0x0001\n"), vec!["TEXT_REF"]);
    assert_eq!(codes("cutscene 16\n"), vec!["CUTSCENE_RANGE"]);
    assert_eq!(codes("if_flag 4\nblock_begin\nsay @0x0D4C\nblock_end\n"), vec!["BLOCK_NO_ELSE"]);
    assert_eq!(codes("goto nowhere\n"), vec!["ASM_LABEL_UNKNOWN"]);
    assert_eq!(codes("end\nsay @0x0D4C\n"), vec!["ASM_ENDS_EARLY"]);
    assert!(codes("if flag(4) {\n  say @0x0D4C\n} else {\n  end\n}\nwait 10\n").is_empty(), "an end inside a block is fine");
}
