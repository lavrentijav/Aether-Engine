//! The neutral scoreboard/sidebar model and its per-protocol encoding.
//!
//! A sidebar is a scoreboard objective shown in the `sidebar` display slot:
//! a title plus up to fifteen lines. The plugin tier never builds packets; it
//! hands the core a [`SidebarBoard`] and the core renders it in each client's
//! own dialect.
//!
//! The core's job is the *mechanism*: the vanilla scoreboard packets
//! (`scoreboard_objective`, `scoreboard_score`, `scoreboard_display_objective`,
//! `teams`, `reset_score`, and the tab-list header/footer), parameterized by
//! the two shapes the versions split into — teams carrying the visible text
//! (up to 1.20.2) and the score's own display name (1.20.3+). The *policy* —
//! which lines, resolved from config and placeholders, and when to resend —
//! lives in the sidebar plugin.
//!
//! **Flicker-free.** A line is a stable score entry. Its text lives in a
//! team's prefix/suffix on the team-carrying versions, and in the score's
//! display name on 1.20.3+. A change rewrites only that entry, so the
//! objective, its display slot and the other lines are never torn down and
//! rebuilt; [`present`] diffs two boards and emits only what changed.

use crate::proto::PacketOut;

use super::nbt;

/// The `sidebar` display slot id, the same in every version.
pub const SIDEBAR_SLOT: i8 = 1;

/// The objective name the sidebar uses. Kept clear of the vanilla `/scoreboard`
/// names so the two can coexist (and short enough for 1.8's 16-char limit).
pub const OBJECTIVE: &str = "aether.side";

/// The most lines a sidebar shows (vanilla allows fifteen).
pub const MAX_LINES: usize = 15;

/// One player's sidebar: a title and its lines, plain text with legacy `§`
/// formatting codes. Empty strings are kept as blank lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SidebarBoard {
    pub title: String,
    pub lines: Vec<String>,
}

impl SidebarBoard {
    /// A board from a title and raw lines, clipped to the fifteen a sidebar
    /// shows. Extra lines are dropped rather than wrapping: a sixteenth line
    /// is invisible on the client, not drawn.
    pub fn new(title: impl Into<String>, lines: Vec<String>) -> SidebarBoard {
        let mut lines = lines;
        lines.truncate(MAX_LINES);
        SidebarBoard {
            title: title.into(),
            lines,
        }
    }
}

/// How a codec can draw a sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarStyle {
    /// No scoreboard packets on this wire: the plugin's updates are dropped.
    Unsupported,
    /// The line text rides in a team's prefix (and suffix when long); the
    /// score entry itself is an invisible colour code. Up to 1.20.2.
    Teams,
    /// The line text rides in the score's own display name and the red number
    /// is hidden with `number_format = blank`. 1.20.3+.
    DisplayNames,
}

/// Which line length a team's prefix and suffix may hold, in characters.
#[derive(Debug, Clone, Copy)]
pub struct TeamWidth {
    /// The prefix limit: 16 on 1.8, 64 from 1.13.
    pub prefix: usize,
    /// The suffix limit: 16 on 1.8, 64 from 1.13.
    pub suffix: usize,
}

/// One explicit scoreboard packet, already resolved by [`present`]. A codec
/// turns it into its version's bytes; it carries no version policy of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScoreboardOp {
    /// Create the objective (action 0).
    ObjectiveAdd { name: String, display: String },
    /// Rename the objective (action 2).
    ObjectiveDisplay { name: String, display: String },
    /// Remove the objective (action 1).
    ObjectiveRemove { name: String },
    /// Point a display slot at an objective, or clear it with `None`.
    Display { slot: i8, objective: Option<String> },
    /// Create a team (mode 0) with its members.
    TeamAdd {
        name: String,
        prefix: String,
        suffix: String,
        members: Vec<String>,
    },
    /// Rewrite a team's prefix/suffix (mode 2).
    TeamUpdate {
        name: String,
        prefix: String,
        suffix: String,
    },
    /// Remove a team (mode 1).
    TeamRemove { name: String },
    /// Set a score. `display` is the score's own display name (1.20.3+ only).
    ScoreSet {
        objective: String,
        entry: String,
        value: i32,
        display: Option<String>,
        /// Whether the red number should be hidden (1.20.3+ only).
        hide_numbers: bool,
    },
    /// Remove a score.
    ResetScore { objective: String, entry: String },
    /// The tab list header and footer, plain text with `§` codes.
    TabHeaderFooter { header: String, footer: String },
}

/// The entry name for line `i`: an invisible run of colour codes on the
/// team-carrying versions, where the scoreboard draws the entry itself, and a
/// stable identifier on 1.20.3+, where the display name replaces it.
fn entry_name(i: usize) -> String {
    // `§0`..`§e` are fifteen distinct, invisible owners; the client renders a
    // bare colour code as no glyph, so only the team prefix shows.
    const DIGITS: &[u8] = b"0123456789abcde";
    format!("\u{00a7}{}", DIGITS.get(i).copied().unwrap_or(b'f') as char)
}

/// The team name for line `i`.
fn team_name(i: usize) -> String {
    format!("sbl{i}")
}

/// The score that puts line `i` at its place: line 0 on top, `MAX_LINES` at
/// the bottom.
fn line_score(i: usize) -> i32 {
    (MAX_LINES - i) as i32
}

/// Diff two sidebar states into the packets that move from `prev` to `next`.
///
/// `None` is "hidden". Only the changed lines produce a packet; an unchanged
/// tick returns an empty `Vec` and writes nothing to the socket, which is what
/// keeps a periodically re-evaluated sidebar from flickering.
pub fn present(
    style: SidebarStyle,
    prev: Option<&SidebarBoard>,
    next: Option<&SidebarBoard>,
) -> Vec<ScoreboardOp> {
    if style == SidebarStyle::Unsupported {
        return Vec::new();
    }
    match (prev, next) {
        (_, None) => hide(style, prev),
        (None, Some(next)) => show(style, next),
        (Some(prev), Some(next)) => update(style, prev, next),
    }
}

/// Turn the sidebar on: create the objective, display it, then add the lines.
fn show(style: SidebarStyle, board: &SidebarBoard) -> Vec<ScoreboardOp> {
    let mut ops = vec![
        ScoreboardOp::ObjectiveAdd {
            name: OBJECTIVE.to_string(),
            display: board.title.clone(),
        },
        ScoreboardOp::Display {
            slot: SIDEBAR_SLOT,
            objective: Some(OBJECTIVE.to_string()),
        },
    ];
    for (i, line) in board.lines.iter().enumerate() {
        match style {
            SidebarStyle::Teams => {
                ops.push(ScoreboardOp::ScoreSet {
                    objective: OBJECTIVE.to_string(),
                    entry: entry_name(i),
                    value: line_score(i),
                    display: None,
                    hide_numbers: true,
                });
                ops.push(ScoreboardOp::TeamAdd {
                    name: team_name(i),
                    prefix: line.clone(),
                    suffix: String::new(),
                    members: vec![entry_name(i)],
                });
            }
            SidebarStyle::DisplayNames => {
                ops.push(ScoreboardOp::ScoreSet {
                    objective: OBJECTIVE.to_string(),
                    entry: format!("sbl{i}"),
                    value: line_score(i),
                    display: Some(line.clone()),
                    hide_numbers: true,
                });
            }
            SidebarStyle::Unsupported => {}
        }
    }
    ops
}

/// Turn the sidebar off: clear the slot, then remove the objective and the
/// lines' teams so nothing is left behind for the next objective to inherit.
fn hide(style: SidebarStyle, prev: Option<&SidebarBoard>) -> Vec<ScoreboardOp> {
    if prev.is_none() {
        return Vec::new();
    }
    let prev = prev.unwrap();
    let mut ops = vec![
        ScoreboardOp::Display {
            slot: SIDEBAR_SLOT,
            objective: None,
        },
        ScoreboardOp::ObjectiveRemove {
            name: OBJECTIVE.to_string(),
        },
    ];
    for i in 0..prev.lines.len() {
        match style {
            SidebarStyle::Teams => ops.push(ScoreboardOp::TeamRemove { name: team_name(i) }),
            SidebarStyle::DisplayNames => ops.push(ScoreboardOp::ResetScore {
                objective: OBJECTIVE.to_string(),
                entry: format!("sbl{i}"),
            }),
            SidebarStyle::Unsupported => {}
        }
    }
    ops
}

/// A live sidebar changed: rename if the title did, then only the lines that
/// differ, plus the ones added or dropped.
fn update(style: SidebarStyle, prev: &SidebarBoard, next: &SidebarBoard) -> Vec<ScoreboardOp> {
    let mut ops = Vec::new();
    if prev.title != next.title {
        ops.push(ScoreboardOp::ObjectiveDisplay {
            name: OBJECTIVE.to_string(),
            display: next.title.clone(),
        });
    }
    let common = prev.lines.len().min(next.lines.len());
    for i in 0..common {
        if prev.lines[i] == next.lines[i] {
            continue;
        }
        match style {
            SidebarStyle::Teams => ops.push(ScoreboardOp::TeamUpdate {
                name: team_name(i),
                prefix: next.lines[i].clone(),
                suffix: String::new(),
            }),
            SidebarStyle::DisplayNames => ops.push(ScoreboardOp::ScoreSet {
                objective: OBJECTIVE.to_string(),
                entry: format!("sbl{i}"),
                value: line_score(i),
                display: Some(next.lines[i].clone()),
                hide_numbers: true,
            }),
            SidebarStyle::Unsupported => {}
        }
    }
    // Lines that appeared.
    for (i, line) in next.lines.iter().enumerate().skip(common) {
        match style {
            SidebarStyle::Teams => {
                ops.push(ScoreboardOp::ScoreSet {
                    objective: OBJECTIVE.to_string(),
                    entry: entry_name(i),
                    value: line_score(i),
                    display: None,
                    hide_numbers: true,
                });
                ops.push(ScoreboardOp::TeamAdd {
                    name: team_name(i),
                    prefix: line.clone(),
                    suffix: String::new(),
                    members: vec![entry_name(i)],
                });
            }
            SidebarStyle::DisplayNames => ops.push(ScoreboardOp::ScoreSet {
                objective: OBJECTIVE.to_string(),
                entry: format!("sbl{i}"),
                value: line_score(i),
                display: Some(line.clone()),
                hide_numbers: true,
            }),
            SidebarStyle::Unsupported => {}
        }
    }
    // Lines that disappeared.
    for i in common..prev.lines.len() {
        match style {
            SidebarStyle::Teams => ops.push(ScoreboardOp::TeamRemove { name: team_name(i) }),
            SidebarStyle::DisplayNames => ops.push(ScoreboardOp::ResetScore {
                objective: OBJECTIVE.to_string(),
                entry: format!("sbl{i}"),
            }),
            SidebarStyle::Unsupported => {}
        }
    }
    ops
}

// --- Wire encoding -----------------------------------------------------

/// The four scoreboard packet ids of one version (plus `reset`), and how its
/// fields are shaped.
#[derive(Debug, Clone, Copy)]
pub struct Wire {
    /// `scoreboard_objective`.
    pub objective: i32,
    /// `scoreboard_score`.
    pub score: i32,
    /// `scoreboard_display_objective`.
    pub display: i32,
    /// `teams`.
    pub team: i32,
    /// `reset_score`, or `-1` on versions that remove a score through the
    /// score packet's action `1` instead (up to 1.20.2).
    pub reset: i32,
    /// How text components are spelled in this version's packets.
    pub text: TextForm,
    /// The shape of the `teams` packet.
    pub team_form: TeamForm,
    /// The display slot: a signed byte before 1.20.2, a VarInt from 1.20.2.
    pub display_pos: PosForm,
    /// Whether the score packet carries an action field (`0` set, `1` reset).
    /// True up to 1.20.2; from 1.20.3 the packet always sets and reset moved
    /// to its own packet.
    pub score_action: bool,
    /// Whether the objective packet's criterion is a string (`"integer"`, 1.8)
    /// or a VarInt. And whether the objective carries a `number_format`.
    pub criterion_string: bool,
    /// Whether the objective and score packets carry `number_format` and the
    /// score carries a `display_name` (1.20.3+).
    pub number_format: bool,
}

/// How a version spells a text component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextForm {
    /// A raw string of legacy `§` codes (1.8).
    Legacy,
    /// A JSON chat component `{"text":"…"}` (1.13–1.20.2).
    Json,
    /// An anonymous NBT string tag (1.20.3+).
    Nbt,
}

/// The shape of the `teams` packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamForm {
    /// 1.8: `name, prefix, suffix, friendlyFire, nameTagVisibility, color`;
    /// prefix and suffix are 16-character strings.
    Legacy,
    /// 1.13–1.20.2: `name, friendlyFire, nameTagVisibility, collisionRule,
    /// color, prefix, suffix, players`; strings, up to 64 characters.
    Classic,
    /// 1.20.3–1.21.8: `Classic`, but name/prefix/suffix are NBT components.
    Nbt,
    /// 1.21.9+: `Nbt`, but friendly-fire is a flag byte and visibility and
    /// collision are VarInt enums.
    NbtFlags,
}

/// Whether the display slot is a signed byte or a VarInt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PosForm {
    Byte,
    Varint,
}

impl Wire {
    /// The 1.8 wire: raw `§` strings, 16-character team halves, no number
    /// format, and a `string` criterion.
    pub fn legacy_1_8() -> Wire {
        Wire {
            objective: 0x3B,
            score: 0x3C,
            display: 0x3D,
            team: 0x3E,
            reset: -1,
            text: TextForm::Legacy,
            team_form: TeamForm::Legacy,
            display_pos: PosForm::Byte,
            score_action: true,
            criterion_string: true,
            number_format: false,
        }
    }

    /// The 1.13–1.20.2 wire: JSON text, 64-character team halves, an action in
    /// the score packet, and the display slot as a byte (`1.20.2` used a
    /// VarInt, so `display_varint` distinguishes the two).
    pub fn json(objective: i32, score: i32, display: i32, team: i32, display_varint: bool) -> Wire {
        Wire {
            objective,
            score,
            display,
            team,
            reset: -1,
            text: TextForm::Json,
            team_form: TeamForm::Classic,
            display_pos: if display_varint {
                PosForm::Varint
            } else {
                PosForm::Byte
            },
            score_action: true,
            criterion_string: false,
            number_format: false,
        }
    }

    /// The 1.20.3+ wire: NBT text with a score display name and a number
    /// format, a dedicated reset-score packet, and (1.21.9+) a flags-based
    /// team packet.
    #[allow(clippy::too_many_arguments)]
    pub fn modern(
        objective: i32,
        score: i32,
        display: i32,
        team: i32,
        reset: i32,
        team_form: TeamForm,
    ) -> Wire {
        Wire {
            objective,
            score,
            display,
            team,
            reset,
            text: TextForm::Nbt,
            team_form,
            display_pos: PosForm::Varint,
            score_action: false,
            criterion_string: false,
            number_format: true,
        }
    }

    /// The team prefix/suffix width this version allows.
    pub fn team_width(&self) -> TeamWidth {
        match self.team_form {
            TeamForm::Legacy => TeamWidth {
                prefix: 16,
                suffix: 16,
            },
            _ => TeamWidth {
                prefix: 64,
                suffix: 64,
            },
        }
    }
}

/// Encode one operation into `wire`'s packets.
pub fn encode(wire: &Wire, op: &ScoreboardOp) -> Vec<PacketOut> {
    match op {
        ScoreboardOp::ObjectiveAdd { name, display } => {
            let mut p = PacketOut::new(wire.objective);
            p.string(name).u8(0);
            write_text(&mut p, wire.text, display);
            write_criterion(&mut p, wire);
            vec![p]
        }
        ScoreboardOp::ObjectiveDisplay { name, display } => {
            let mut p = PacketOut::new(wire.objective);
            p.string(name).u8(2);
            write_text(&mut p, wire.text, display);
            write_criterion(&mut p, wire);
            vec![p]
        }
        ScoreboardOp::ObjectiveRemove { name } => {
            let mut p = PacketOut::new(wire.objective);
            p.string(name).u8(1);
            vec![p]
        }
        ScoreboardOp::Display { slot, objective } => {
            let mut p = PacketOut::new(wire.display);
            write_slot(&mut p, wire.display_pos, *slot);
            p.string(objective.as_deref().unwrap_or(""));
            vec![p]
        }
        ScoreboardOp::TeamAdd {
            name,
            prefix,
            suffix,
            members,
        } => vec![write_team(wire, name, 0, prefix, suffix, members)],
        ScoreboardOp::TeamUpdate {
            name,
            prefix,
            suffix,
        } => vec![write_team(wire, name, 2, prefix, suffix, &[])],
        ScoreboardOp::TeamRemove { name } => {
            let mut p = PacketOut::new(wire.team);
            p.string(name).u8(1);
            vec![p]
        }
        ScoreboardOp::ScoreSet {
            objective,
            entry,
            value,
            display,
            hide_numbers,
        } => {
            let mut p = PacketOut::new(wire.score);
            p.string(objective);
            if wire.score_action {
                p.var_int(0); // create/update
                p.string(entry).var_int(*value);
            } else {
                p.string(entry).var_int(*value);
                if wire.number_format {
                    match display {
                        Some(text) => {
                            p.bool(true);
                            p.bytes(&nbt::string(text).to_network());
                        }
                        None => {
                            p.bool(false);
                        }
                    }
                    if *hide_numbers {
                        p.bool(true).var_int(0); // blank
                    } else {
                        p.bool(false);
                    }
                    // Styling is only written when number_format is 1 or 2
                    // (styled/fixed); blank carries none.
                }
            }
            vec![p]
        }
        ScoreboardOp::ResetScore { objective, entry } => {
            let mut p = PacketOut::new(if wire.reset >= 0 {
                wire.reset
            } else {
                wire.score
            });
            p.string(objective);
            if wire.reset < 0 {
                p.var_int(1); // remove
            }
            p.string(entry);
            vec![p]
        }
        ScoreboardOp::TabHeaderFooter { header, footer } => {
            // Encoded by the tab-list codec path, not the scoreboard one; the
            // sidebar service never sends this op through here.
            let _ = (header, footer);
            Vec::new()
        }
    }
}

/// Write a text component in `form`.
fn write_text(p: &mut PacketOut, form: TextForm, text: &str) {
    match form {
        TextForm::Legacy => {
            p.string(text);
        }
        TextForm::Json => {
            p.string(&format!("{{\"text\":\"{}\"}}", super::json_escape(text)));
        }
        TextForm::Nbt => {
            p.bytes(&nbt::string(text).to_network());
        }
    }
}

/// The objective's criterion and, from 1.20.3, its number format.
fn write_criterion(p: &mut PacketOut, wire: &Wire) {
    if wire.criterion_string {
        p.string("integer");
    } else {
        p.var_int(0); // integer
    }
    if wire.number_format {
        // `number_format = blank` hides the red numbers the sidebar would
        // otherwise draw beside each line.
        p.bool(true).var_int(0);
    }
}

/// Write a display slot in the version's width.
fn write_slot(p: &mut PacketOut, form: PosForm, slot: i8) {
    match form {
        PosForm::Byte => {
            p.u8(slot as u8);
        }
        PosForm::Varint => {
            p.var_int(slot as i32);
        }
    }
}

/// Write one team packet: `mode` 0 add, 2 change.
fn write_team(
    wire: &Wire,
    name: &str,
    mode: u8,
    prefix: &str,
    suffix: &str,
    members: &[String],
) -> PacketOut {
    let width = wire.team_width();
    let (prefix, suffix) = if matches!(wire.team_form, TeamForm::Legacy) {
        split_line(prefix, suffix, width)
    } else {
        (
            truncate(prefix, width.prefix),
            truncate(suffix, width.suffix),
        )
    };
    let mut p = PacketOut::new(wire.team);
    p.string(name).u8(mode);

    let named = mode == 0 || mode == 2;
    if named {
        match wire.team_form {
            TeamForm::Legacy => {
                // 1.8 order: name, prefix, suffix, friendlyFire,
                // nameTagVisibility, color.
                p.string(&prefix).string(&suffix);
                p.u8(0); // no friendly fire
                p.string("always");
                p.u8(0xFF); // no colour override
            }
            TeamForm::Classic => {
                p.string(name);
                p.u8(0); // no friendly fire
                p.string("always");
                p.string("always"); // collision rule
                p.var_int(21); // formatting: reset
                p.string(&prefix).string(&suffix);
            }
            TeamForm::Nbt => {
                write_text(&mut p, TextForm::Nbt, name);
                p.u8(0); // no friendly fire
                p.string("always");
                p.string("always");
                p.var_int(21);
                write_text(&mut p, TextForm::Nbt, &prefix);
                write_text(&mut p, TextForm::Nbt, &suffix);
            }
            TeamForm::NbtFlags => {
                write_text(&mut p, TextForm::Nbt, name);
                p.u8(0); // flags: neither friendly-fire nor see-invisible
                p.var_int(0); // name tag visibility: always
                p.var_int(0); // collision rule: always
                p.var_int(21); // formatting: reset
                write_text(&mut p, TextForm::Nbt, &prefix);
                write_text(&mut p, TextForm::Nbt, &suffix);
            }
        }
    }
    if mode == 0 {
        p.var_int(members.len() as i32);
        for m in members {
            p.string(m);
        }
    }
    p
}

/// The scoreboard wire for a protocol id, or `None` where the version has no
/// scoreboard packets (nothing this server speaks). The one table that knows
/// every version's four ids; the codecs themselves just report their protocol.
///
/// The ids are from PrismarineJS `minecraft-data` (`pc/<version>/protocol.json`),
/// packet `scoreboard_objective` / `scoreboard_score` /
/// `scoreboard_display_objective` / `teams` / `reset_score`.
pub fn wire_for(protocol: i32) -> Option<Wire> {
    Some(match protocol {
        47 => Wire::legacy_1_8(),
        // 1.17 – 1.18.2: JSON text, byte display slot.
        755..=758 => Wire::json(0x53, 0x56, 0x4c, 0x55, false),
        // 1.19: same ids as 1.17; 1.19.2 shifted them.
        759 => Wire::json(0x53, 0x56, 0x4c, 0x55, false),
        760 => Wire::json(0x56, 0x59, 0x4f, 0x58, false),
        761 => Wire::json(0x54, 0x57, 0x4d, 0x56, false),
        // 1.19.4 and 1.20/1.20.1.
        762 | 763 => Wire::json(0x58, 0x5b, 0x51, 0x5a, false),
        // 1.20.2: still JSON, but the display slot became a VarInt.
        764 => Wire::json(0x5a, 0x5d, 0x53, 0x5c, true),
        // 1.20.3 – 1.20.6: NBT text with number formats; team strings are NBT.
        765 => Wire::modern(0x5c, 0x5f, 0x55, 0x5e, 0x42, TeamForm::Nbt),
        766 | 767 => Wire::modern(0x5e, 0x61, 0x57, 0x60, 0x44, TeamForm::Nbt),
        // 1.21.2 – 1.21.4.
        768 | 769 => Wire::modern(0x64, 0x68, 0x5c, 0x67, 0x49, TeamForm::Nbt),
        // 1.21.5 – 1.21.8.
        770..=772 => Wire::modern(0x63, 0x67, 0x5b, 0x66, 0x48, TeamForm::Nbt),
        // 1.21.9+ : the team packet gained a flags byte and VarInt enums.
        773 | 774 => Wire::modern(0x68, 0x6c, 0x60, 0x6b, 0x4d, TeamForm::NbtFlags),
        // 26.1–26.3. Packet ids are unverified here (no data for them in the
        // generator's reports), so they reuse 1.21.11's; the field *shapes*
        // are correct, and a wrong id would be corrected by regenerating this
        // table from the release's own `packets.json`.
        775..=777 => Wire::modern(0x68, 0x6c, 0x60, 0x6b, 0x4d, TeamForm::NbtFlags),
        _ => return None,
    })
}

/// Split a long line into the prefix and suffix a 1.8 team can hold, each at
/// most `width` characters and never cutting a `§` from the code it opens.
pub fn split_line(text: &str, suffix: &str, width: TeamWidth) -> (String, String) {
    let mut prefix = String::new();
    for c in text.chars() {
        if prefix.chars().count() >= width.prefix {
            break;
        }
        prefix.push(c);
    }
    // A trailing lone `§` would colour nothing and swallow the next line's
    // first character; move it to the suffix.
    if prefix.ends_with('\u{00a7}') {
        prefix.pop();
    }
    let rest: String = text.chars().skip(prefix.chars().count()).collect();
    let combined = if rest.is_empty() {
        suffix.to_string()
    } else {
        rest
    };
    let mut suffix_out = String::new();
    for c in combined.chars() {
        if suffix_out.chars().count() >= width.suffix {
            break;
        }
        suffix_out.push(c);
    }
    if suffix_out.ends_with('\u{00a7}') {
        suffix_out.pop();
    }
    (prefix, suffix_out)
}

/// Truncate to at most `max` characters, honouring a trailing `§`.
fn truncate(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if out.ends_with('\u{00a7}') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(title: &str, lines: &[&str]) -> SidebarBoard {
        SidebarBoard::new(title, lines.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn a_fresh_board_creates_the_objective_then_its_lines() {
        let b = board("&6Title", &["one", "two"]);
        let ops = present(SidebarStyle::Teams, None, Some(&b));
        assert!(matches!(ops[0], ScoreboardOp::ObjectiveAdd { .. }));
        assert!(matches!(ops[1], ScoreboardOp::Display { slot: 1, .. }));
        // Score before team for each line.
        assert!(matches!(ops[2], ScoreboardOp::ScoreSet { .. }));
        assert!(matches!(ops[3], ScoreboardOp::TeamAdd { .. }));
        assert!(matches!(ops[4], ScoreboardOp::ScoreSet { .. }));
        assert!(matches!(ops[5], ScoreboardOp::TeamAdd { .. }));
    }

    #[test]
    fn an_unchanged_board_writes_nothing() {
        // The heart of flicker-free: re-presenting the same board is silent.
        let b = board("&6Title", &["one", "two", "three"]);
        let ops = present(SidebarStyle::Teams, Some(&b), Some(&b));
        assert!(ops.is_empty(), "no change must mean no packets");
        let ops = present(SidebarStyle::DisplayNames, Some(&b), Some(&b));
        assert!(ops.is_empty());
    }

    #[test]
    fn one_changed_line_is_one_packet() {
        let prev = board("&6Title", &["one", "two", "three"]);
        let next = board("&6Title", &["one", "TWO", "three"]);
        let ops = present(SidebarStyle::Teams, Some(&prev), Some(&next));
        assert_eq!(ops.len(), 1, "only the changed line is resent");
        match &ops[0] {
            ScoreboardOp::TeamUpdate { name, prefix, .. } => {
                assert_eq!(name, "sbl1");
                assert_eq!(prefix, "TWO");
            }
            other => panic!("expected a team update, got {other:?}"),
        }
        let ops = present(SidebarStyle::DisplayNames, Some(&prev), Some(&next));
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            ScoreboardOp::ScoreSet { entry, display, .. } => {
                assert_eq!(entry, "sbl1");
                assert_eq!(display.as_deref(), Some("TWO"));
            }
            other => panic!("expected a score set, got {other:?}"),
        }
    }

    #[test]
    fn a_title_change_only_renames_the_objective() {
        let prev = board("&6Old", &["one"]);
        let next = board("&6New", &["one"]);
        let ops = present(SidebarStyle::Teams, Some(&prev), Some(&next));
        assert_eq!(
            ops,
            vec![ScoreboardOp::ObjectiveDisplay {
                name: OBJECTIVE.to_string(),
                display: "&6New".to_string(),
            }]
        );
    }

    #[test]
    fn a_line_that_disappears_removes_its_team_and_score() {
        let prev = board("t", &["one", "two", "three"]);
        let next = board("t", &["one", "two"]);
        let teams = present(SidebarStyle::Teams, Some(&prev), Some(&next));
        assert_eq!(
            teams,
            vec![ScoreboardOp::TeamRemove {
                name: "sbl2".to_string()
            }]
        );
        let names = present(SidebarStyle::DisplayNames, Some(&prev), Some(&next));
        assert_eq!(
            names,
            vec![ScoreboardOp::ResetScore {
                objective: OBJECTIVE.to_string(),
                entry: "sbl2".to_string()
            }]
        );
    }

    #[test]
    fn the_fifteen_line_cap_is_enforced() {
        let lines: Vec<String> = (0..40).map(|i| i.to_string()).collect();
        let b = SidebarBoard::new("t", lines);
        assert_eq!(b.lines.len(), MAX_LINES);
    }

    #[test]
    fn hiding_clears_the_slot_and_the_objective() {
        let b = board("t", &["one", "two"]);
        let ops = present(SidebarStyle::Teams, Some(&b), None);
        assert_eq!(
            ops[0],
            ScoreboardOp::Display {
                slot: SIDEBAR_SLOT,
                objective: None
            }
        );
        assert!(ops.contains(&ScoreboardOp::ObjectiveRemove {
            name: OBJECTIVE.to_string()
        }));
        assert!(ops.contains(&ScoreboardOp::TeamRemove {
            name: "sbl0".to_string()
        }));
        // Hiding an already-hidden sidebar writes nothing.
        assert!(present(SidebarStyle::Teams, None, None).is_empty());
    }

    #[test]
    fn an_unsupported_codec_gets_no_ops() {
        let b = board("t", &["one"]);
        assert!(present(SidebarStyle::Unsupported, None, Some(&b)).is_empty());
        assert!(present(SidebarStyle::Unsupported, Some(&b), None).is_empty());
    }

    #[test]
    fn a_long_1_8_line_is_split_into_prefix_and_suffix() {
        let text = "0123456789abcdefghijklmnopqrstuvwxyz"; // 36 chars
        let width = TeamWidth {
            prefix: 16,
            suffix: 16,
        };
        let (prefix, suffix) = split_line(text, "", width);
        assert_eq!(prefix.chars().count(), 16);
        assert_eq!(suffix.chars().count(), 16);
        assert_eq!(prefix, "0123456789abcdef");
        assert_eq!(suffix, "ghijklmnopqrstuv");
    }

    #[test]
    fn splitting_never_leaves_a_dangling_colour_code() {
        // The cut lands right on the `§`; it must move, not split.
        let text = "123456789012345\u{00a7}6red";
        let width = TeamWidth {
            prefix: 16,
            suffix: 16,
        };
        let (prefix, _suffix) = split_line(text, "", width);
        assert!(!prefix.ends_with('\u{00a7}'), "no dangling § in the prefix");
    }

    // --- Wire encoding -------------------------------------------------

    /// The style the codec serving `protocol` reports.
    fn style_for(protocol: i32) -> SidebarStyle {
        for c in crate::protocol::codecs() {
            if c.supports(protocol) {
                return c.sidebar_style();
            }
        }
        SidebarStyle::Unsupported
    }

    #[test]
    fn the_wire_table_knows_every_spoken_protocol() {
        assert_eq!(wire_for(47).unwrap().objective, 0x3B);
        assert_eq!(wire_for(47).unwrap().team, 0x3E);
        assert_eq!(wire_for(755).unwrap().objective, 0x53);
        assert_eq!(wire_for(760).unwrap().objective, 0x56);
        assert_eq!(wire_for(764).unwrap().objective, 0x5A);
        assert_eq!(wire_for(765).unwrap().reset, 0x42);
        assert_eq!(wire_for(774).unwrap().objective, 0x68);
        assert_eq!(wire_for(774).unwrap().team, 0x6B);
        assert_eq!(wire_for(774).unwrap().reset, 0x4D);
        // A protocol we do not speak has no scoreboard wire.
        assert!(wire_for(999).is_none());
    }

    #[test]
    fn the_style_follows_the_number_format() {
        // Only 1.20.3+ has a number format, and that is exactly the split
        // between the two sidebar strategies.
        assert_eq!(style_for(47), SidebarStyle::Teams, "1.8");
        assert_eq!(style_for(755), SidebarStyle::Teams, "1.17");
        assert_eq!(style_for(764), SidebarStyle::Teams, "1.20.2");
        assert_eq!(style_for(765), SidebarStyle::DisplayNames, "1.20.3");
        assert_eq!(style_for(774), SidebarStyle::DisplayNames, "1.21.11");
    }

    #[test]
    fn a_1_8_objective_uses_a_string_criterion() {
        let wire = wire_for(47).unwrap();
        let ops = encode(
            &wire,
            &ScoreboardOp::ObjectiveAdd {
                name: "aether.side".to_string(),
                display: "§6Hi".to_string(),
            },
        );
        let mut pin = crate::proto::PacketIn::new(ops[0].body());
        assert_eq!(pin.var_int().unwrap(), 0x3B);
        assert_eq!(pin.string().unwrap(), "aether.side");
        assert_eq!(pin.u8().unwrap(), 0, "action: add");
        assert_eq!(pin.string().unwrap(), "§6Hi", "1.8 carries a raw string");
        assert_eq!(
            pin.string().unwrap(),
            "integer",
            "the criterion is a string"
        );
    }

    #[test]
    fn a_1_8_score_has_an_action_field() {
        let wire = wire_for(47).unwrap();
        let ops = encode(
            &wire,
            &ScoreboardOp::ScoreSet {
                objective: "aether.side".to_string(),
                entry: "§0".to_string(),
                value: 15,
                display: None,
                hide_numbers: true,
            },
        );
        let mut pin = crate::proto::PacketIn::new(ops[0].body());
        assert_eq!(pin.var_int().unwrap(), 0x3C);
        assert_eq!(pin.string().unwrap(), "aether.side");
        assert_eq!(pin.var_int().unwrap(), 0, "create/update");
        assert_eq!(pin.string().unwrap(), "§0");
        assert_eq!(pin.var_int().unwrap(), 15);
    }

    #[test]
    fn a_1_8_reset_uses_the_score_packet_action() {
        // 1.8 has no reset-score packet; the score packet's action 1 does it.
        let wire = wire_for(47).unwrap();
        let ops = encode(
            &wire,
            &ScoreboardOp::ResetScore {
                objective: "aether.side".to_string(),
                entry: "§0".to_string(),
            },
        );
        let mut pin = crate::proto::PacketIn::new(ops[0].body());
        assert_eq!(pin.var_int().unwrap(), 0x3C);
        assert_eq!(pin.string().unwrap(), "aether.side");
        assert_eq!(pin.var_int().unwrap(), 1, "remove");
        assert_eq!(pin.string().unwrap(), "§0");
    }

    #[test]
    fn a_json_objective_wraps_the_text_in_a_component() {
        let wire = wire_for(755).unwrap();
        let ops = encode(
            &wire,
            &ScoreboardOp::ObjectiveAdd {
                name: "aether.side".to_string(),
                display: "§6Hi".to_string(),
            },
        );
        let mut pin = crate::proto::PacketIn::new(ops[0].body());
        assert_eq!(pin.var_int().unwrap(), 0x53);
        let _name = pin.string().unwrap();
        assert_eq!(pin.u8().unwrap(), 0);
        assert_eq!(pin.string().unwrap(), "{\"text\":\"§6Hi\"}");
        assert_eq!(pin.var_int().unwrap(), 0, "criterion is a VarInt here");
    }

    #[test]
    fn a_modern_objective_carries_nbt_text_and_a_blank_number_format() {
        let wire = wire_for(774).unwrap();
        let ops = encode(
            &wire,
            &ScoreboardOp::ObjectiveAdd {
                name: "aether.side".to_string(),
                display: "§6Hi".to_string(),
            },
        );
        let body = ops[0].body();
        let mut pin = crate::proto::PacketIn::new(body);
        assert_eq!(pin.var_int().unwrap(), 0x68);
        let _name = pin.string().unwrap();
        assert_eq!(pin.u8().unwrap(), 0);
        // Anonymous NBT: tag byte 8 (TAG_String), then a 2-byte length, then
        // the text itself (`§6Hi` in UTF-8).
        assert_eq!(pin.u8().unwrap(), 8, "TAG_String");
        let len = pin.u16().unwrap() as usize;
        assert_eq!(len, 5, "the text's byte length");
        assert!(
            body.windows(5).any(|w| w == "§6Hi".as_bytes()),
            "the NBT string carries the title"
        );
        pin.skip(len).unwrap();
        // ... then the criterion VarInt and `number_format = blank`.
        assert_eq!(pin.var_int().unwrap(), 0, "criterion: integer");
        assert!(pin.bool().unwrap(), "a number format is present");
        assert_eq!(pin.var_int().unwrap(), 0, "blank hides the red number");
    }

    #[test]
    fn a_modern_score_carries_its_display_name() {
        let wire = wire_for(774).unwrap();
        let ops = encode(
            &wire,
            &ScoreboardOp::ScoreSet {
                objective: "aether.side".to_string(),
                entry: "sbl0".to_string(),
                value: 15,
                display: Some("Line".to_string()),
                hide_numbers: true,
            },
        );
        let mut pin = crate::proto::PacketIn::new(ops[0].body());
        assert_eq!(pin.var_int().unwrap(), 0x6C);
        assert_eq!(pin.string().unwrap(), "aether.side");
        assert_eq!(pin.string().unwrap(), "sbl0");
        assert_eq!(pin.var_int().unwrap(), 15);
        assert!(pin.bool().unwrap(), "a display name follows");
        assert_eq!(pin.u8().unwrap(), 8, "TAG_String");
        let len = pin.u16().unwrap() as usize;
        assert_eq!(len, 4, "\"Line\"");
        pin.skip(len).unwrap();
        // After the display-name NBT comes `number_format = blank`:
        // present (1) then blank (0).
        assert!(pin.bool().unwrap(), "a number format is present");
        assert_eq!(pin.var_int().unwrap(), 0, "blank");
    }

    #[test]
    fn a_modern_reset_score_has_its_own_packet() {
        let wire = wire_for(774).unwrap();
        let ops = encode(
            &wire,
            &ScoreboardOp::ResetScore {
                objective: "aether.side".to_string(),
                entry: "sbl0".to_string(),
            },
        );
        let mut pin = crate::proto::PacketIn::new(ops[0].body());
        assert_eq!(pin.var_int().unwrap(), 0x4D);
        assert_eq!(pin.string().unwrap(), "aether.side");
        assert_eq!(pin.string().unwrap(), "sbl0");
    }

    #[test]
    fn a_1_8_team_splits_a_long_line_into_prefix_and_suffix() {
        let wire = wire_for(47).unwrap();
        let text = "0123456789abcdefghijklmnopqrstuvwxyz";
        let ops = encode(
            &wire,
            &ScoreboardOp::TeamUpdate {
                name: "sbl0".to_string(),
                prefix: text.to_string(),
                suffix: String::new(),
            },
        );
        let mut pin = crate::proto::PacketIn::new(ops[0].body());
        assert_eq!(pin.var_int().unwrap(), 0x3E);
        assert_eq!(pin.string().unwrap(), "sbl0");
        assert_eq!(pin.u8().unwrap(), 2, "change");
        // 1.8 order: name, prefix, suffix, ...
        assert_eq!(pin.string().unwrap(), "0123456789abcdef");
        assert_eq!(pin.string().unwrap(), "ghijklmnopqrstuv");
    }

    #[test]
    fn a_modern_team_uses_the_flags_byte() {
        // 1.21.9 moved friendly-fire into a flag byte and visibility and
        // collision into VarInt enums; a classic-layout packet would shift.
        let wire = wire_for(774).unwrap();
        let ops = encode(
            &wire,
            &ScoreboardOp::TeamAdd {
                name: "sbl0".to_string(),
                prefix: "line".to_string(),
                suffix: String::new(),
                members: vec!["§0".to_string()],
            },
        );
        let mut pin = crate::proto::PacketIn::new(ops[0].body());
        assert_eq!(pin.var_int().unwrap(), 0x6B);
        assert_eq!(pin.string().unwrap(), "sbl0");
        assert_eq!(pin.u8().unwrap(), 0, "add");
        assert_eq!(pin.u8().unwrap(), 8, "display name is TAG_String");
        let len = pin.u16().unwrap() as usize;
        pin.skip(len).unwrap();
        assert_eq!(pin.u8().unwrap(), 0, "flags: none set");
        assert_eq!(pin.var_int().unwrap(), 0, "visibility: always");
        assert_eq!(pin.var_int().unwrap(), 0, "collision: always");
    }

    #[test]
    fn an_old_team_packet_keeps_the_1_8_field_order() {
        // The 1.8 team packet has no collision rule and puts prefix before
        // the friendly-fire byte; the modern one puts them after.
        let wire = wire_for(47).unwrap();
        let ops = encode(
            &wire,
            &ScoreboardOp::TeamAdd {
                name: "sbl0".to_string(),
                prefix: "line".to_string(),
                suffix: String::new(),
                members: vec!["§0".to_string()],
            },
        );
        let mut pin = crate::proto::PacketIn::new(ops[0].body());
        assert_eq!(pin.var_int().unwrap(), 0x3E);
        assert_eq!(pin.string().unwrap(), "sbl0");
        assert_eq!(pin.u8().unwrap(), 0, "add");
        assert_eq!(pin.string().unwrap(), "line", "1.8: prefix comes first");
        assert_eq!(pin.string().unwrap(), "", "then suffix");
        assert_eq!(pin.u8().unwrap(), 0, "then friendly fire");
        assert_eq!(pin.string().unwrap(), "always", "then name-tag visibility");
        assert_eq!(pin.u8().unwrap(), 0xFF, "then colour");
        assert_eq!(pin.var_int().unwrap(), 1, "one member");
        assert_eq!(pin.string().unwrap(), "§0");
    }
}
