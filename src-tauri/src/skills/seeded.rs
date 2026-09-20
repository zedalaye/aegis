//! The runbooks this build seeds into the library, and seeding them (PLAN 7.6,
//! Phase 19). The bodies are `seed/<name>.md`, embedded at build time.

use std::fs;
use std::path::Path;

use super::SKILL_FILE;

// ---------------------------------------------------------------------------
// The library on disk
// ---------------------------------------------------------------------------

/// Creates the library and offers each seeded runbook once per name, recorded
/// in [`SEEDED_FILE`], so a deleted runbook never comes back and a new build's
/// runbook still reaches old installs. Best effort.
///
/// A library older than the manifest counts a [`SEEDED_BEFORE`] name as offered
/// only if its directory exists: wrongly assuming "offered" would lose a
/// runbook silently, while wrongly assuming "not offered" only restores an
/// example once.
///
/// The seeds are the mode's own runbooks (review, `cos.loop`, cabinet founding,
/// the four world skills) and the Phase 19 domain packs — see
/// `docs/guide/packs.md` for what each pack declares and why. Seeding grants
/// nothing (PLAN 7.6, *Authoring*).
pub fn seed(library: &Path) {
    let manifest = library.join(SEEDED_FILE);
    let mut offered: Vec<String> = match fs::read_to_string(&manifest) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        // A library from before the manifest: infer from its disk.
        Err(_) if library.is_dir() => SEEDED_BEFORE
            .iter()
            .filter(|name| library.join(name).is_dir())
            .map(|&name| (*name).to_owned())
            .collect(),
        Err(_) => Vec::new(),
    };

    let mut written = 0usize;
    for (name, body) in SEEDED {
        if offered.iter().any(|seen| seen == name) {
            continue;
        }

        let dir = library.join(name);
        if let Err(err) = fs::create_dir_all(&dir) {
            tracing::warn!(%err, dir = %dir.display(), "could not create the skill library");
            return;
        }
        if let Err(err) = fs::write(dir.join(SKILL_FILE), body) {
            tracing::warn!(%err, name, "could not write an example skill");
            return;
        }
        offered.push((*name).to_owned());
        written += 1;
    }

    if written == 0 {
        return;
    }
    // After the runbooks: a crash in between costs a redundant write, not a
    // lost runbook.
    if let Err(err) = fs::write(&manifest, format!("{}\n", offered.join("\n"))) {
        tracing::warn!(%err, path = %manifest.display(), "could not record what was seeded");
    }
    tracing::info!(dir = %library.display(), written, "example runbooks seeded");
}

/// The library's record of runbooks already offered, one name per line.
pub(super) const SEEDED_FILE: &str = ".seeded";

/// What a library older than [`SEEDED_FILE`] **may** have been offered. Early
/// builds skipped existing libraries, so some never got [`COS_SKILL`]; [`seed`]
/// checks the disk.
pub(super) const SEEDED_BEFORE: [&str; 2] = [REVIEW_SKILL, COS_SKILL];

/// Every runbook this build seeds, and the body each starts as.
pub(super) const SEEDED: [(&str, &str); 25] = [
    (REVIEW_SKILL, REVIEW_SEED),
    (COS_SKILL, COS_SEED),
    (FOUND_SKILL, FOUND_SEED),
    (DRAFT_SKILL, DRAFT_SEED),
    (PERCEIVE_SKILL, PERCEIVE_SEED),
    (VERIFY_SKILL, VERIFY_SEED),
    (CHECK_SKILL, CHECK_SEED),
    (REVIEW_DIFF_SKILL, REVIEW_DIFF_SEED),
    (DEPLOY_SKILL, DEPLOY_SEED),
    (ALERT_SKILL, ALERT_SEED),
    (MAIL_SKILL, MAIL_SEED),
    (THREAD_SKILL, THREAD_SEED),
    (REPLY_SKILL, REPLY_SEED),
    (WATCH_SWEEP_SKILL, WATCH_SWEEP_SEED),
    (WATCH_DIGEST_SKILL, WATCH_DIGEST_SEED),
    (WATCH_IMPACT_SKILL, WATCH_IMPACT_SEED),
    (BUDGET_POSITION_SKILL, BUDGET_POSITION_SEED),
    (BUDGET_RUNWAY_SKILL, BUDGET_RUNWAY_SEED),
    (BUDGET_ALERT_SKILL, BUDGET_ALERT_SEED),
    (SOCIAL_SCAN_SKILL, SOCIAL_SCAN_SEED),
    (SOCIAL_REPLY_SKILL, SOCIAL_REPLY_SEED),
    (SOCIAL_POST_SKILL, SOCIAL_POST_SEED),
    (WISH_LIST_SKILL, WISH_LIST_SEED),
    (REVENUE_THESIS_SKILL, REVENUE_THESIS_SEED),
    (REVENUE_PIPELINE_SKILL, REVENUE_PIPELINE_SEED),
];

/// The standing rule of the whole mode, as a runbook.
pub const REVIEW_SKILL: &str = "never-send-without-review";

/// `never-send-without-review`: the mode's standing rule, and an example that
/// needs no connector (PLAN 7.6, *Verifier is a skill*).
pub(super) const REVIEW_SEED: &str = include_str!("seed/never-send-without-review.md");

/// The Chief of Staff's own loop (PLAN 7.3, Phase 15).
pub const COS_SKILL: &str = "cos.loop";

/// `cos.loop`: `COS.md` *Loop* as a runbook rather than prompt text
/// (PLAN 7.1), so it costs one catalog line and its owner can edit it.
pub(super) const COS_SEED: &str = include_str!("seed/cos.loop.md");

/// Founding a cabinet (PLAN 7.14).
pub const FOUND_SKILL: &str = "cabinet.found";

/// `cabinet.found`: writes `.aegis/roster/PROPOSAL.md` and creates nobody; a
/// person applies it in Settings ([`roster`](crate::roster)). Its steps carry
/// the packs' declared tools so proposed grants do not fail closed; its example
/// is indented so a `## ` is not read as a section by [`doc::parse`].
pub const FOUND_SEED: &str = include_str!("seed/cabinet.found.md");

/// Help with founding or amending a world (PLAN 7.2).
pub const DRAFT_SKILL: &str = "world.draft";

/// `world.draft`: the only seeded runbook that writes `world/`, for an attended
/// session (refused inside a brief, PLAN 7.2). It drafts the descriptive files
/// from evidence and never invents the essence or the oracle.
pub(super) const DRAFT_SEED: &str = include_str!("seed/world.draft.md");

/// The delta half of the world's library (PLAN 7.2, *Library skills*).
pub const PERCEIVE_SKILL: &str = "world.perceive-delta";

/// `world.perceive-delta`: reads only the declared sources that moved and
/// returns a proposal; never writes `world/`.
pub(super) const PERCEIVE_SEED: &str = include_str!("seed/world.perceive-delta.md");

/// The verifier half of the world's library (PLAN 7.2, *Library skills*).
pub const VERIFY_SKILL: &str = "world.verify";

/// `world.verify`: checks an instance against the oracle, with paths as
/// evidence (`COS.md` *Work*).
pub(super) const VERIFY_SEED: &str = include_str!("seed/world.verify.md");

/// The world's own read, for a session the frame did not open
/// (PLAN 7.2, *Library skills*).
pub const CHECK_SKILL: &str = "world.check";

/// `world.check`: a cheap, read-only pass over the constitution before touching
/// a world.
pub(super) const CHECK_SEED: &str = include_str!("seed/world.check.md");

// ---------------------------------------------------------------------------
// The client-delivery pack (PLAN 7.3, Phase 19, pack 1)
// ---------------------------------------------------------------------------

/// Review a range before it goes to a client (PLAN 7.6 names it, under *Three
/// scopes*).
pub const REVIEW_DIFF_SKILL: &str = "review.diff";

/// `review.diff`: reads a range via `git` and asks the same four questions in a
/// fixed order, reading surrounding files, not just hunks.
pub(super) const REVIEW_DIFF_SEED: &str = include_str!("seed/review.diff.md");

/// Draft a deployment somebody else runs (PLAN 7.3, Phase 19: *destructive
/// deploy stays gated*).
pub const DEPLOY_SKILL: &str = "deploy.draft";

/// `deploy.draft`: everything up to the deploy, which stays human (PLAN 7.4).
/// Facts come from the project's own files; without them it is `blocked`.
pub(super) const DEPLOY_SEED: &str = include_str!("seed/deploy.draft.md");

/// Turn a monitoring signal into a note and a reply nobody has sent.
pub const ALERT_SKILL: &str = "alert.draft";

/// `alert.draft`: an incident note separating observed (with commands) from
/// inferred, plus an unsent client reply carrying no guessed cause. Never
/// restarts anything.
pub(super) const ALERT_SEED: &str = include_str!("seed/alert.draft.md");

// ---------------------------------------------------------------------------
// The client-intake pack (PLAN 7.3, Phase 19, pack 2)
// ---------------------------------------------------------------------------

/// Turn one inbound message into a ticket (PLAN 7.3, Phase 19: *mail first —
/// read + draft, never send*).
pub const MAIL_SKILL: &str = "mail.triage";

/// `mail.triage`: one message file into a ticket. The ask must be a quoted,
/// dated sentence so *no ask* is possible; money or access requests are
/// `needs_you`. Step 1 handles `.eml` attachments, which can fill or overflow
/// [`READ_MAX_BYTES`](crate::tools::READ_MAX_BYTES).
pub(super) const MAIL_SEED: &str = include_str!("seed/mail.triage.md");

/// Work out where a conversation actually stands.
pub const THREAD_SKILL: &str = "thread.recap";

/// `thread.recap`: where a thread stands, de-duplicating quoted text. Its
/// agreed/outstanding split (silence is not agreement) feeds [`REPLY_SEED`].
pub(super) const THREAD_SEED: &str = include_str!("seed/thread.recap.md");

/// Draft the answer somebody else sends (`COS.md` *Loop*; PLAN 7.4).
pub const REPLY_SKILL: &str = "reply.draft";

/// `reply.draft`: a reply a person sends (PLAN 7.4). Every date, price and
/// scope cites its file; an unsourced commitment goes to `open_questions`. It
/// never picks its own input.
pub(super) const REPLY_SEED: &str = include_str!("seed/reply.draft.md");

/// Turn what arrived into entries (PLAN 7.3, Phase 19, pack 3).
pub const WATCH_SWEEP_SKILL: &str = "watch.sweep";

/// `watch.sweep`: files in the workspace into entries that keep *what it says*
/// apart from *what it shows*. What was swept is the set of entry files; delete
/// one to re-read it.
pub(super) const WATCH_SWEEP_SEED: &str = include_str!("seed/watch.sweep.md");

/// Report what is new since the last report (PLAN 7.3, Phase 19, pack 3).
pub const WATCH_DIGEST_SKILL: &str = "watch.digest";

/// `watch.digest`: written for a clock. A quiet period writes nothing and
/// returns `done`, not `blocked`, so the routine does not pause itself
/// (PLAN 7.6). Each digest lists the entries it covered, so the next run reads
/// only what is new.
pub(super) const WATCH_DIGEST_SEED: &str = include_str!("seed/watch.digest.md");

/// What one entry would mean here (PLAN 7.3, Phase 19, pack 3).
pub const WATCH_IMPACT_SKILL: &str = "watch.impact";

/// `watch.impact`: what one given entry would mean here, as conditions to check,
/// with the cost of doing nothing too. A needed `world/` change is reported as
/// an écart, never made ([`Grant::WorldAmend`] is refused to routines).
///
/// [`Grant::WorldAmend`]: crate::policy::Grant::WorldAmend
pub(super) const WATCH_IMPACT_SEED: &str = include_str!("seed/watch.impact.md");

/// What is held and what is owed (PLAN 7.3, Phase 19, pack 4).
pub const BUDGET_POSITION_SKILL: &str = "budget.position";

/// `budget.position`: what is held and owed. Every figure is copied from a
/// source line or shown as redoable arithmetic reconciled to stated totals, and
/// the file leads with its stalest as-of date.
pub(super) const BUDGET_POSITION_SEED: &str = include_str!("seed/budget.position.md");

/// How long the money lasts (PLAN 7.3, Phase 19, pack 4).
pub const BUDGET_RUNWAY_SKILL: &str = "budget.runway";

/// `budget.runway`: a range, not a point, with annual commitments spread
/// monthly. Never picks its own input.
pub(super) const BUDGET_RUNWAY_SEED: &str = include_str!("seed/budget.runway.md");

/// A line was crossed (PLAN 7.3, Phase 19, pack 4).
pub const BUDGET_ALERT_SKILL: &str = "budget.alert";

/// `budget.alert`: reports a threshold somebody else set being crossed, and
/// stops before any order or recommendation to trade (PLAN 7.3, 7.4).
pub(super) const BUDGET_ALERT_SEED: &str = include_str!("seed/budget.alert.md");

/// Which of it, if any, is worth answering (PLAN 7.3, Phase 19, pack 5).
pub const SOCIAL_SCAN_SKILL: &str = "social.scan";

/// `social.scan`: the few posts worth answering against a criteria file, with
/// *none worth answering* as an ordinary result. "Someone is wrong" is never a
/// criterion.
pub(super) const SOCIAL_SCAN_SEED: &str = include_str!("seed/social.scan.md");

/// Draft the answer to one post (PLAN 7.3, Phase 19, pack 5).
pub const SOCIAL_REPLY_SKILL: &str = "social.reply";

/// `social.reply`: one public, permanent answer. The steps name the shapes to
/// refuse (gratuitous corrections, openings about the other person being wrong)
/// and end by handing off to [`REVIEW_SKILL`].
pub(super) const SOCIAL_REPLY_SEED: &str = include_str!("seed/social.reply.md");

/// Say the thing that happened (PLAN 7.3, Phase 19, pack 5).
pub const SOCIAL_POST_SKILL: &str = "social.post";

/// `social.post`: every claim names something already done and on disk, each
/// sentence must survive being quoted alone, and nothing promises the future.
pub(super) const SOCIAL_POST_SEED: &str = include_str!("seed/social.post.md");

/// What somebody wants, written where it can be seen (PLAN 7.3, Phase 19,
/// pack 6).
pub const WISH_LIST_SKILL: &str = "wish.list";

/// `wish.list`: somebody's goals as a file, never a memory (PLAN 7.4). Nothing
/// acquires the grammar of a fact: unstated order is *unordered*, an unchecked
/// price is *not priced*. No judgement of the wants.
pub(super) const WISH_LIST_SEED: &str = include_str!("seed/wish.list.md");

/// One idea, written well enough to be wrong (PLAN 7.3, Phase 19, pack 6).
pub const REVENUE_THESIS_SKILL: &str = "revenue.thesis";

/// `revenue.thesis`: a falsifiable proposal with what would disprove it and what
/// being wrong costs — no size, allocation or expected return. It may not read
/// the position or the wish list (see [`REVENUE_PIPELINE_SEED`]).
pub(super) const REVENUE_THESIS_SEED: &str = include_str!("seed/revenue.thesis.md");

/// What is funded, what is not (PLAN 7.3, Phase 19, pack 6).
pub const REVENUE_PIPELINE_SKILL: &str = "revenue.pipeline";

/// `revenue.pipeline`: shows wants and proposals side by side and the gap between
/// them, never claiming a proposal closes it (PLAN 7.3). The two prefixes keep
/// this apart from [`REVENUE_THESIS_SEED`].
pub(super) const REVENUE_PIPELINE_SEED: &str = include_str!("seed/revenue.pipeline.md");

/// `inbox.triage`, the workspace example runbook (Phase 13): a brief file in,
/// status and artefact out.
pub const TRIAGE_SEED: &str = include_str!("seed/inbox.triage.md");
