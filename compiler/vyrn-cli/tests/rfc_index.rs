//! `rfcs/README.md` is derived from `rfcs/`, or it is wrong.
//!
//! WHAT THIS REPLACES. The index was hand-maintained and had drifted in three
//! ways at once: it said "97 RFCs, numbered 0001 to 0098" when there were 99
//! through 0100, and RFC-0099 and RFC-0100 — both shipped and merged — had no
//! row at all. That is the same defect the corpus has been removing everywhere
//! else: a fact about a directory, restated in a file, drifting away from it.
//!
//! A design record that contradicts the code is worse than no record, and an
//! index that has never heard of two RFCs is the version of that a reader hits
//! first. So the four facts the index states about the directory are checked
//! against the directory here:
//!
//!   1. **Membership, both ways.** Every `.md` file in `rfcs/` except the README
//!      is linked from the README, and every `.md` the README links to exists.
//!      One set comparison covers the RFC table and the "other documents" table
//!      together.
//!   2. **The row agrees with the file.** Each index row is `| [NNNN](FILE) |`,
//!      and `FILE` must be the RFC numbered `NNNN`. Exactly one row per RFC.
//!   3. **The count and the range.** The prose sentence "N RFCs, numbered AAAA
//!      to BBBB" is parsed and compared to the directory, and the gaps it
//!      declares ("There is no RFC-NNNN") must be exactly the numbers missing
//!      from that range.
//!   4. **A cross-reference resolves.** Every `RFC-NNNN` named anywhere in
//!      `rfcs/*.md` is either a file that exists or one of the gaps computed in
//!      (3). This is what keeps a banner honest: a banner that sends the reader
//!      to the RFC that replaced a mechanism is only useful if that RFC is
//!      there.
//!
//! WHAT THIS DOES NOT CHECK, deliberately.
//!
//! - **Status text.** The README says its own rule — "Each RFC carries its own
//!   `**Status:**` header, and that header is the authority. This index copies
//!   the header; it does not judge it." Copying is exactly the thing that
//!   drifts, so this is the obvious next check. It is not here because the
//!   headers are prose, not a closed set (they run to two sentences and hold
//!   per-milestone detail the one-line table deliberately compresses), so a
//!   byte comparison would fail on every honest summary and a fuzzy one would
//!   pass on a wrong one. Gating it means first deciding what a status IS.
//! - **Titles.** Same reason, weaker stakes.
//! - **Whether a banner is accurate.** No test can read an RFC and a compiler
//!   and say the two agree. This checks that the pointer resolves, not that the
//!   claim at the end of it is true.
//! - **A link into a section** (`RFC-0089-…md#rule-4`): the file is checked, the
//!   anchor is not.
//!
//! CI NOTE. `ci.yml` carries `paths-ignore: ['rfcs/**', '**.md', 'editor/**']`,
//! so a commit touching ONLY `rfcs/` runs none of its jobs. This gate runs on
//! every pull request from `.github/workflows/docs.yml` instead, which has no
//! path filter (RFC-0125 M0): a docs-only PR reports this check and nothing
//! else. The workspace suite runs it a second time on every commit that builds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The repository root — two levels up from `compiler/vyrn-cli`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn rfcs_dir() -> PathBuf {
    repo_root().join("rfcs")
}

/// Every `.md` file directly under `rfcs/`, by file name, sorted.
fn markdown_files() -> BTreeSet<String> {
    let out: BTreeSet<String> = std::fs::read_dir(rfcs_dir())
        .expect("read rfcs/")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect();
    assert!(
        out.len() > 50,
        "rfcs/ has only {} markdown files?",
        out.len()
    );
    out
}

/// `RFC-0093-a-take-….md` -> `93`. `None` for anything else in the directory.
fn rfc_number(name: &str) -> Option<u32> {
    name.strip_prefix("RFC-")?
        .get(..4)
        .filter(|d| d.len() == 4 && d.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|d| d.parse().ok())
}

/// The RFCs on disk: number -> file name.
fn rfcs_on_disk() -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    for name in markdown_files() {
        if let Some(n) = rfc_number(&name) {
            assert!(
                out.insert(n, name.clone()).is_none(),
                "two files claim RFC-{n:04}"
            );
        }
    }
    out
}

/// Every markdown link target in `src` that names a `.md` file, with any
/// `#anchor` stripped. Deliberately crude: `](` up to the next `)`.
fn md_link_targets(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, _) in src.match_indices("](") {
        let rest = &src[i + 2..];
        let Some(end) = rest.find(')') else { continue };
        let target = rest[..end].split('#').next().unwrap_or("");
        if target.ends_with(".md") && !target.contains('/') {
            out.insert(target.to_string());
        }
    }
    out
}

fn readme() -> String {
    std::fs::read_to_string(rfcs_dir().join("README.md")).expect("read rfcs/README.md")
}

/// (1) Every file in the directory is linked from the index, and every file the
///     index links to is in the directory.
#[test]
fn the_index_links_every_file_in_the_directory_and_no_others() {
    let mut on_disk = markdown_files();
    on_disk.remove("README.md");
    let linked = md_link_targets(&readme());

    let missing: Vec<_> = on_disk.difference(&linked).collect();
    let dangling: Vec<_> = linked.difference(&on_disk).collect();

    assert!(
        missing.is_empty(),
        "rfcs/README.md does not link {} file(s) that exist: {missing:?}",
        missing.len()
    );
    assert!(
        dangling.is_empty(),
        "rfcs/README.md links {} file(s) that do not exist: {dangling:?}",
        dangling.len()
    );
}

/// (2) The index table has exactly one row per RFC, and each row's number and
///     file agree with each other.
#[test]
fn every_index_row_names_the_rfc_it_links_to() {
    let on_disk = rfcs_on_disk();
    let src = readme();
    let mut rows: BTreeMap<u32, String> = BTreeMap::new();

    for line in src.lines() {
        // `| [0093](RFC-0093-….md) | Title | Status |`
        let Some(rest) = line.strip_prefix("| [") else {
            continue;
        };
        let Some((num, rest)) = rest.split_once("](") else {
            continue;
        };
        let Some((file, _)) = rest.split_once(')') else {
            continue;
        };
        let Ok(n) = num.parse::<u32>() else { continue };
        assert!(
            rows.insert(n, file.to_string()).is_none(),
            "RFC-{n:04} has more than one row in the index"
        );
    }

    let table: BTreeSet<u32> = rows.keys().copied().collect();
    let disk: BTreeSet<u32> = on_disk.keys().copied().collect();
    let unlisted: Vec<_> = disk.difference(&table).map(|n| format!("{n:04}")).collect();
    let invented: Vec<_> = table.difference(&disk).map(|n| format!("{n:04}")).collect();

    assert!(
        unlisted.is_empty(),
        "the index table has no row for RFC(s) {unlisted:?} — they exist in rfcs/"
    );
    assert!(
        invented.is_empty(),
        "the index table has a row for RFC(s) {invented:?} — no such file"
    );

    for (n, file) in &rows {
        assert_eq!(
            on_disk.get(n),
            Some(file),
            "the row for RFC-{n:04} links `{file}`, which is not the file numbered {n:04}"
        );
    }
}

/// (3) "N RFCs, numbered AAAA to BBBB" and the gaps it declares.
#[test]
fn the_stated_count_range_and_gaps_match_the_directory() {
    let on_disk = rfcs_on_disk();
    let src = readme();

    let sentence = src
        .lines()
        .find(|l| l.contains("RFCs, numbered "))
        .expect("rfcs/README.md states no count sentence (`N RFCs, numbered AAAA to BBBB`)");

    let count: usize = sentence
        .split_whitespace()
        .next()
        .and_then(|w| w.parse().ok())
        .unwrap_or_else(|| panic!("cannot read a count out of: {sentence}"));
    let (_, tail) = sentence.split_once("numbered ").unwrap();
    let (lo, tail) = tail.split_once(" to ").expect("`AAAA to BBBB`");
    let hi: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    let lo: u32 = lo.trim().parse().expect("a low RFC number");
    let hi: u32 = hi.parse().expect("a high RFC number");

    assert_eq!(
        count,
        on_disk.len(),
        "rfcs/README.md says {count} RFCs; rfcs/ holds {}",
        on_disk.len()
    );
    assert_eq!(
        (lo, hi),
        (
            *on_disk.keys().next().unwrap(),
            *on_disk.keys().next_back().unwrap()
        ),
        "rfcs/README.md says the range is {lo:04}..{hi:04}; the directory's is {:04}..{:04}",
        on_disk.keys().next().unwrap(),
        on_disk.keys().next_back().unwrap()
    );

    // A gap is derived, not written down: a number inside the range with no
    // file. The README must name each one, and must not claim one that exists.
    let gaps = gap_numbers(&on_disk);
    let declared = src.matches("There is no RFC-").count();
    assert_eq!(
        declared,
        gaps.len(),
        "rfcs/README.md declares {declared} gap(s); the directory has {} ({:?})",
        gaps.len(),
        gaps.iter().map(|n| format!("{n:04}")).collect::<Vec<_>>()
    );
    for n in &gaps {
        assert!(
            src.contains(&format!("There is no RFC-{n:04}")),
            "RFC-{n:04} is missing from rfcs/ and the README does not say so"
        );
    }
}

/// The numbers inside the corpus's own range that have no file.
fn gap_numbers(on_disk: &BTreeMap<u32, String>) -> Vec<u32> {
    let lo = *on_disk.keys().next().unwrap();
    let hi = *on_disk.keys().next_back().unwrap();
    (lo..=hi).filter(|n| !on_disk.contains_key(n)).collect()
}

/// (4) Every `RFC-NNNN` named anywhere in `rfcs/*.md` resolves: it is a file
///     that exists, or a gap the README declares.
#[test]
fn every_cross_reference_in_the_corpus_resolves() {
    let on_disk = rfcs_on_disk();
    let gaps: BTreeSet<u32> = gap_numbers(&on_disk).into_iter().collect();
    let mut dangling: BTreeSet<String> = BTreeSet::new();

    for name in markdown_files() {
        let src = std::fs::read_to_string(rfcs_dir().join(&name)).expect("read an rfcs/ file");
        for (i, _) in src.match_indices("RFC-") {
            let digits: String = src[i + 4..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            // `RFC-XXXX` in a template or a prose ellipsis is not a reference.
            if digits.len() != 4 {
                continue;
            }
            let n: u32 = digits.parse().expect("four digits");
            if !on_disk.contains_key(&n) && !gaps.contains(&n) {
                dangling.insert(format!("{name}: RFC-{n:04}"));
            }
        }
        // A link is a stronger claim than a mention: the file must be there.
        for target in md_link_targets(&src) {
            if rfc_number(&target).is_some() {
                assert!(
                    rfcs_dir().join(&target).is_file(),
                    "rfcs/{name} links `{target}`, which does not exist"
                );
            }
        }
    }

    assert!(
        dangling.is_empty(),
        "the corpus names {} RFC(s) that neither exist nor are declared gaps: {dangling:?}",
        dangling.len()
    );
}
