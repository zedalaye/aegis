//! One `SKILL.md`, parsed and judged.
//!
//! A skill is a *frozen how* (PLAN 7.6): a runbook someone wrote down once, so
//! that neither the Chef de Cabinet nor a specialist has to re-derive it in a
//! context window every time. This module is the half of the runner that
//! decides whether a file is one.
//!
//! Two things are checked, and for different reasons.
//!
//! **The front matter, because a catalog has to be machine-readable.** A
//! catalog line carries a version and the tools the runbook will call, and
//! neither can be scraped out of prose without guessing. `tools` in particular
//! is not decoration: it is what lets a run be refused *before* it starts when
//! the identity does not hold one of them, rather than after four rounds of
//! discovering it one refusal at a time (PLAN 7.6, *No extra rights*).
//!
//! **The seven headings of `COS.md` *Skills*, because the runner enforces them
//! and the model does not get to skip one.** They are required, in order, and
//! nothing else is allowed beside them. That is stricter than markdown needs
//! to be, deliberately: the headings are the contract between whoever writes a
//! runbook and whoever runs it, and a file that quietly omitted *what to do if
//! the source is missing* would be a runbook whose failure mode is inventing
//! an answer. A refusal names the heading, so an author is told which line to
//! add rather than that their file is "invalid".
//!
//! A parse failure is never fatal to anything. It makes one catalog entry
//! carry a [`problem`](super::Skill::problem) instead of being runnable, which
//! is what puts the message in front of the person who can fix it.

use crate::tools;

/// The headings every skill declares, in the order it declares them
/// (`COS.md` *Skills*).
///
/// The parenthetical glosses `COS.md` writes after three of them — "(strict
/// format — a handoff result)", "(maps onto the policy matrix …)", "(return
/// `blocked`, do not invent)" — explain the heading rather than being part of
/// it, so a file that copied them verbatim is accepted (see [`normalize`]).
pub const HEADINGS: [&str; 7] = [
    "When to use it",
    "Inputs required and tools it will call",
    "Steps",
    "How to validate",
    "What to return",
    "What requires approval",
    "What to do if the source is missing",
];

/// Front-matter keys this build understands.
///
/// Named in the refusal for an unknown one: "unknown key" without the list is
/// a message that sends the author to the source code.
const KEYS: [&str; 2] = ["version", "tools"];

/// Longest `SKILL.md` the runner will load.
///
/// A cap on the *runbook*, not on what it can reach: a skill points at files
/// and reads them with `fs_read` like anything else. The number is a judgement
/// about what a runbook is — sixteen kilobytes is several pages of steps, and
/// a procedure longer than that is a document that wants splitting into skills
/// which name each other, not one turn's worth of instructions.
pub const BODY_MAX_BYTES: usize = 16 * 1024;

/// Longest catalog line a skill contributes.
///
/// The catalog is in the system message of every turn (PLAN 7.6, *catalog in,
/// body on demand*), so this is the standing per-skill cost of having a
/// library at all.
pub const SUMMARY_MAX_CHARS: usize = 160;

/// Longest declared version string.
const VERSION_MAX_CHARS: usize = 16;

/// A `SKILL.md` that passed.
///
/// Deliberately not the raw text plus accessors: everything downstream reads
/// these four fields, and a type that still held the unparsed file would let a
/// caller go around the checks by re-reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDoc {
    /// What the author versioned it as. Free-form; compared by people.
    pub version: String,
    /// The tools its steps will call, validated against the registry.
    pub tools: Vec<String>,
    /// The first paragraph of *When to use it*, capped — the catalog line.
    pub summary: String,
    /// The runbook itself: everything after the front matter.
    ///
    /// What `skill_run` hands the model, and the only place it ever appears —
    /// never in a system prompt, never in the catalog.
    pub body: String,
}

/// Parses and judges one `SKILL.md`.
///
/// The error is written for the person who wrote the file rather than for a
/// model: it says what is wrong *and* what a working line looks like, because
/// a validator that only says "invalid" leaves the author guessing.
pub fn parse(text: &str) -> Result<SkillDoc, String> {
    if text.len() > BODY_MAX_BYTES {
        return Err(format!(
            "this runbook is {} bytes and the runner loads at most {BODY_MAX_BYTES}. A procedure \
             longer than that wants splitting into skills that name each other, not one turn's \
             worth of instructions",
            text.len()
        ));
    }

    let (front, body) = split(text)?;
    let (version, tools) = front_matter(front)?;
    let sections = sections(body)?;
    let summary = summarize(&sections[0]);

    Ok(SkillDoc {
        version,
        tools,
        summary,
        body: body.trim().to_owned(),
    })
}

/// Splits the leading `---` block from the runbook.
///
/// The fence has to be the very first thing in the file. A front matter that
/// could start anywhere would make "this file has none" indistinguishable from
/// "this file has one further down that I did not notice".
fn split(text: &str) -> Result<(&str, &str), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    // Only a bare `---` line opens it: `----`, or `--- something`, is ordinary
    // text and a file starting with one has no front matter rather than a
    // malformed one.
    let opened = text.strip_prefix("---").and_then(|rest| {
        rest.strip_prefix("\r\n")
            .or_else(|| rest.strip_prefix('\n'))
    });

    let Some(rest) = opened else {
        return Err(
            "a skill starts with a front-matter block: a line of `---`, then `version:` \
                    and `tools:`, then another line of `---`"
                .to_owned(),
        );
    };

    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Ok((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }

    Err(
        "the front-matter block is never closed — add a line of `---` after the last key"
            .to_owned(),
    )
}

/// Reads `version` and `tools` out of the front matter.
fn front_matter(front: &str) -> Result<(String, Vec<String>), String> {
    let mut version: Option<String> = None;
    let mut declared: Vec<String> = Vec::new();

    for line in front.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once(':') else {
            return Err(format!(
                "`{line}` is not a front-matter line. Each one is `key: value`, and the keys are \
                 {}",
                KEYS.join(", ")
            ));
        };
        let value = value.trim();

        match key.trim() {
            "version" => version = Some(value.to_owned()),
            "tools" => {
                declared = value
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            other => {
                return Err(format!(
                    "`{other}` is not a front-matter key. The keys are {}",
                    KEYS.join(", ")
                ))
            }
        }
    }

    let version = version.unwrap_or_default();
    let version = version.trim();
    if version.is_empty() {
        return Err(
            "the front matter needs a `version:` — a skill is a versioned runbook, and a \
                    run nobody can name the version of cannot be replayed"
                .to_owned(),
        );
    }
    if version.chars().count() > VERSION_MAX_CHARS {
        return Err(format!(
            "keep `version:` under {VERSION_MAX_CHARS} characters — it is a label, like `1` or \
             `2026-08-30`"
        ));
    }

    // Kept in registry order and validated against it, for the reason an
    // identity's allow-list is (`store::agents`): the strings the policy table
    // keys on and the audit log records are one vocabulary, so a runbook
    // cannot declare a tool that does not exist and one name cannot mean two
    // things.
    let mut tools = Vec::new();
    for name in tools::names() {
        if declared.iter().any(|wanted| wanted == name) {
            tools.push((*name).to_owned());
        }
    }
    // A runbook may also declare a connector's tool (PLAN 7.3, Phase 18),
    // checked for shape rather than for existence — for the reason an
    // identity's allow-list is: a runbook is a file that travels with a
    // repository, and one that would not parse on a machine where the
    // connector is not installed would be a runbook nobody could read.
    for wanted in declared {
        if tools.iter().any(|known| known == &wanted) {
            continue;
        }
        if crate::store::connectors::split_tool_name(&wanted).is_none() {
            return Err(format!(
                "`{wanted}` is not a tool this build has. The tools are: {}. A connector's tool \
                 is named `<connector>__<tool>`",
                tools::names().join(", ")
            ));
        }
        tools.push(wanted.clone());
    }

    Ok((version.to_owned(), tools))
}

/// Splits the runbook into its seven sections, refusing anything else.
///
/// Returns the text under each heading, in [`HEADINGS`] order.
fn sections(body: &str) -> Result<Vec<String>, String> {
    let mut found: Vec<(String, String)> = Vec::new();

    for line in body.lines() {
        match line.strip_prefix("## ") {
            Some(heading) => found.push((normalize(heading), String::new())),
            None => {
                if let Some((_, text)) = found.last_mut() {
                    text.push_str(line);
                    text.push('\n');
                }
            }
        }
    }

    for (index, expected) in HEADINGS.iter().enumerate() {
        let wanted = normalize(expected);
        match found.get(index) {
            Some((heading, _)) if *heading == wanted => {}
            Some((heading, _)) => {
                return Err(format!(
                    "heading {} should be `## {expected}`; this file has `{heading}` there. The \
                     seven headings are required, in order — they are the contract between \
                     whoever wrote the runbook and whoever runs it",
                    index + 1
                ))
            }
            None => {
                return Err(format!(
                    "this runbook has no `## {expected}` (heading {} of {}). A skill declares all \
                     seven — one with no answer for a missing source is one that invents an answer",
                    index + 1,
                    HEADINGS.len()
                ))
            }
        }
    }

    if let Some((extra, _)) = found.get(HEADINGS.len()) {
        return Err(format!(
            "`{extra}` is an eighth section. A skill has exactly the seven headings; anything \
             else belongs under one of them"
        ));
    }

    Ok(found.into_iter().map(|(_, text)| text).collect())
}

/// A heading as it is compared.
///
/// Case, surrounding whitespace, a trailing colon and a parenthetical gloss
/// are authorial rather than semantic: `COS.md` itself writes three of the
/// seven with a gloss, and a file that copied them verbatim must not be
/// refused for it.
fn normalize(heading: &str) -> String {
    let heading = heading.trim();
    let heading = match heading.find('(') {
        Some(at) => &heading[..at],
        None => heading,
    };

    heading
        .trim()
        .trim_end_matches([':', '.', '!', '?'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The catalog line: the first paragraph of *When to use it*, capped.
///
/// Taken from the heading rather than from a second front-matter key, so a
/// skill says when it applies in one place and the catalog cannot drift from
/// the runbook. Bullets are folded into the line — a catalog entry is one
/// line, and a list rendered into a system message would be a list of lists.
fn summarize(section: &str) -> String {
    let paragraph: Vec<&str> = section
        .lines()
        .map(str::trim)
        .skip_while(|line| line.is_empty())
        .take_while(|line| !line.is_empty())
        .map(|line| line.trim_start_matches(['-', '*', '#']).trim())
        .collect();

    let joined = paragraph.join(" ");
    let mut summary = String::new();
    for word in joined.split_whitespace() {
        if summary.chars().count() + word.chars().count() + 1 > SUMMARY_MAX_CHARS {
            summary.push('…');
            break;
        }
        if !summary.is_empty() {
            summary.push(' ');
        }
        summary.push_str(word);
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::policy::tool;

    /// A file that passes, so a test can break one thing and assert on it.
    fn skill(front: &str, headings: &[&str]) -> String {
        let body: String = headings
            .iter()
            .map(|heading| format!("## {heading}\nsomething\n\n"))
            .collect();
        format!("---\n{front}\n---\n\n# demo\n\n{body}")
    }

    fn whole() -> String {
        skill("version: 1\ntools: fs_read, fs_write", &HEADINGS)
    }

    #[test]
    fn a_complete_runbook_parses() {
        let doc = parse(&whole()).expect("accepted");

        assert_eq!(doc.version, "1");
        assert_eq!(doc.tools, vec![tool::FS_READ, tool::FS_WRITE]);
        assert_eq!(doc.summary, "something");
        assert!(doc.body.starts_with("# demo"), "{}", doc.body);
        assert!(
            !doc.body.contains("version:"),
            "the front matter is not part of the runbook"
        );
    }

    /// The seven headings are the contract. A file missing one is refused, and
    /// the refusal names which — an author told "invalid" has to guess.
    #[test]
    fn every_heading_is_required_and_the_refusal_names_it() {
        for (dropped, missing) in HEADINGS.iter().enumerate() {
            let kept: Vec<&str> = HEADINGS
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != dropped)
                .map(|(_, heading)| *heading)
                .collect();

            let err = parse(&skill("version: 1", &kept)).expect_err("refused");
            assert!(
                err.contains(missing),
                "dropping `{missing}` should be named: {err}"
            );
        }
    }

    #[test]
    fn the_headings_are_required_in_order() {
        let mut swapped: Vec<&str> = HEADINGS.to_vec();
        swapped.swap(2, 3);

        let err = parse(&skill("version: 1", &swapped)).expect_err("refused");
        assert!(err.contains("Steps"), "{err}");
    }

    /// `COS.md` writes three of the seven with a parenthetical gloss. A file
    /// that copied them is the same file.
    #[test]
    fn a_gloss_after_a_heading_is_not_part_of_it() {
        let glossed = [
            "When to use it",
            "Inputs required and tools it will call",
            "Steps",
            "How to validate",
            "What to return (strict format — a handoff result)",
            "What requires approval (maps onto the policy matrix)",
            "What to do if the source is missing (return `blocked`, do not invent)",
        ];

        parse(&skill("version: 1", &glossed)).expect("accepted");
    }

    #[test]
    fn an_eighth_section_is_refused() {
        let mut extra: Vec<&str> = HEADINGS.to_vec();
        extra.push("Notes");

        let err = parse(&skill("version: 1", &extra)).expect_err("refused");
        assert!(err.contains("eighth"), "{err}");
    }

    #[test]
    fn a_runbook_needs_a_version() {
        let err = parse(&skill("tools: fs_read", &HEADINGS)).expect_err("refused");
        assert!(err.contains("version"), "{err}");
    }

    /// A skill cannot declare a tool this build does not have: the runner
    /// checks the declaration against the identity's grants before it runs
    /// anything, and a name nothing answers to could never be checked.
    #[test]
    fn a_tool_the_build_does_not_have_is_refused_by_name() {
        let err = parse(&skill("version: 1\ntools: send_email", &HEADINGS)).expect_err("refused");

        assert!(err.contains("send_email"), "{err}");
        assert!(err.contains(tool::FS_READ), "the tools are named: {err}");
    }

    #[test]
    fn an_unknown_front_matter_key_is_refused_with_the_ones_that_work() {
        let err = parse(&skill("version: 1\nowner: me", &HEADINGS)).expect_err("refused");

        assert!(err.contains("owner"), "{err}");
        assert!(err.contains("version"), "{err}");
        assert!(err.contains("tools"), "{err}");
    }

    #[test]
    fn a_file_with_no_front_matter_says_what_one_looks_like() {
        let err = parse("# demo\n\n## When to use it\n").expect_err("refused");
        assert!(err.contains("---"), "{err}");
    }

    #[test]
    fn a_runbook_longer_than_the_cap_is_refused_rather_than_truncated() {
        let padding = "x".repeat(BODY_MAX_BYTES);
        let err = parse(&skill("version: 1", &HEADINGS).replace("# demo", &padding))
            .expect_err("refused");

        assert!(err.contains(&BODY_MAX_BYTES.to_string()), "{err}");
    }

    /// The catalog line comes off the first heading, so there is one place a
    /// skill says when it applies.
    #[test]
    fn the_summary_is_the_first_paragraph_of_when_to_use_it() {
        let text = "---\nversion: 1\n---\n\n## When to use it\n- Before anything leaves\n- the \
                    machine\n\nNot this paragraph.\n\n## Inputs required and tools it will \
                    call\n\n## Steps\n\n## How to validate\n\n## What to return\n\n## What \
                    requires approval\n\n## What to do if the source is missing\n";

        let doc = parse(text).expect("accepted");
        assert_eq!(doc.summary, "Before anything leaves the machine");
    }

    #[test]
    fn a_long_summary_is_capped_and_says_so() {
        let long = "word ".repeat(200);
        let text = format!(
            "---\nversion: 1\n---\n\n## When to use it\n{long}\n\n## Inputs required and tools \
             it will call\n\n## Steps\n\n## How to validate\n\n## What to return\n\n## What \
             requires approval\n\n## What to do if the source is missing\n"
        );

        let doc = parse(&text).expect("accepted");
        assert!(
            doc.summary.chars().count() <= SUMMARY_MAX_CHARS + 1,
            "{}",
            doc.summary
        );
        assert!(doc.summary.ends_with('…'), "{}", doc.summary);
    }
}
