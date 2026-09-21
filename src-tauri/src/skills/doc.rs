//! One `SKILL.md`, parsed and judged.
//!
//! * **Front matter**: `version` and `tools`, so the catalog is machine-readable
//!   and a run can fail closed before starting (PLAN 7.6); optionally
//!   `writes`, the folders its writes land in, which is what signing it onto a
//!   routine offers (PLAN 7.23).
//! * **The seven `COS.md` *Skills* headings**, required in order and nothing
//!   else; a refusal names the missing heading.
//!
//! A failure makes the entry carry a [`problem`](super::Skill::problem).

use crate::tools;

/// The headings every skill declares, in the order it declares them
/// (`COS.md` *Skills*).
///
/// `COS.md`'s parenthetical glosses are accepted too (see [`normalize`]).
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
const KEYS: [&str; 3] = ["version", "tools", "writes"];

/// Longest `SKILL.md` the runner will load.
///
/// A cap on the runbook itself; longer procedures should be split into skills.
pub const BODY_MAX_BYTES: usize = 16 * 1024;

/// Longest catalog line a skill contributes.
///
/// The per-skill cost in every system message (PLAN 7.6).
pub const SUMMARY_MAX_CHARS: usize = 160;

/// Longest declared version string.
const VERSION_MAX_CHARS: usize = 16;

/// A `SKILL.md` that passed.
///
/// Parsed fields only, so nothing downstream can bypass the checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDoc {
    /// What the author versioned it as. Free-form; compared by people.
    pub version: String,
    /// The tools its steps will call, validated against the registry.
    pub tools: Vec<String>,
    /// The folders its `fs_write` calls land in, as write prefixes
    /// ([`narrow::prefix`](crate::policy::narrow::prefix)). Empty when it
    /// does not say.
    pub writes: Vec<String>,
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
/// Errors say what is wrong and what a working line looks like.
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
    let (version, tools, writes) = front_matter(front)?;
    let sections = sections(body)?;
    let summary = summarize(&sections[0]);

    Ok(SkillDoc {
        version,
        tools,
        writes,
        summary,
        body: body.trim().to_owned(),
    })
}

/// Splits the leading `---` block from the runbook.
///
/// The fence must open the file.
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

/// Reads `version`, `tools` and `writes` out of the front matter.
fn front_matter(front: &str) -> Result<(String, Vec<String>, Vec<String>), String> {
    let mut version: Option<String> = None;
    let mut declared: Vec<String> = Vec::new();
    let mut writes: Vec<String> = Vec::new();

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
            "writes" => {
                writes.clear();
                for raw in value
                    .split(',')
                    .map(str::trim)
                    .filter(|raw| !raw.is_empty())
                {
                    let prefix = crate::policy::narrow::prefix(raw)
                        .map_err(|reason| format!("`writes:` {reason}"))?;
                    if !writes.contains(&prefix) {
                        writes.push(prefix);
                    }
                }
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

    // Validated against the registry, in registry order (like `store::agents`).
    let mut tools = Vec::new();
    for name in tools::names() {
        if declared.iter().any(|wanted| wanted == name) {
            tools.push((*name).to_owned());
        }
    }
    // Connector tools (Phase 18) are checked for shape only: the connector may
    // not be installed here.
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

    if !writes.is_empty()
        && !tools
            .iter()
            .any(|name| name == crate::policy::tool::FS_WRITE)
    {
        return Err(
            "`writes:` names where this runbook's writes land, and `tools:` does not list \
             `fs_write`. Declare the tool, or drop `writes:`"
                .to_owned(),
        );
    }

    Ok((version.to_owned(), tools, writes))
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
/// Ignores case, surrounding whitespace, a trailing colon and a parenthetical
/// gloss.
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
/// Taken from the heading, not a separate key; bullets are folded into one
/// line.
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

    /// PLAN 7.23: a runbook may say where its writes land.
    #[test]
    fn writes_are_prefixes_and_need_the_write_tool() {
        let doc = parse(&skill(
            "version: 1
tools: fs_read, fs_write
writes: ./.aegis/artefacts/, notes/drafts-*",
            &HEADINGS,
        ))
        .expect("accepted");
        assert_eq!(doc.writes, vec![".aegis/artefacts", "notes/drafts-*"]);
        assert!(parse(&whole()).expect("accepted").writes.is_empty());

        let refused = parse(&skill(
            "version: 1
tools: fs_read
writes: notes",
            &HEADINGS,
        ))
        .expect_err("refused");
        assert!(refused.contains("fs_write"), "{refused}");
        let world = parse(&skill(
            "version: 1
tools: fs_write
writes: world/essence",
            &HEADINGS,
        ))
        .expect_err("refused");
        assert!(world.contains("world/"), "{world}");
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
