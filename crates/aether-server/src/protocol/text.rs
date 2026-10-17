//! Text components: the chat/UI value, parsed once and rendered per codec.
//!
//! A text component is the value `/tellraw` accepts and every chat line is
//! made of. The server keeps it in one version-neutral form ([`TextComponent`])
//! and each codec spells it in its own dialect:
//!
//! * **1.20.2 and older** send JSON (`{\"text\":\"…\",\"clickEvent\":{…}}`);
//! * **1.20.3 – 1.21.4** send an anonymous NBT component with the old
//!   camel-case style keys;
//! * **1.21.5 and newer** (including 1.21.11 and 26.x) send an anonymous NBT
//!   component with the renamed `click_event` / `hover_event` fields.
//!
//! The command argument is SNBT, so the same parser reads
//! `{text:\"hi\",color:\"red\"}` and `{\"text\":\"hi\",\"color\":\"red\"}`.
//!
//! Parsing is deliberately **total and bounded**: an unknown key is ignored
//! (vanilla's codecs ignore unknown fields), an unknown content kind is an
//! error, a wrong type is an error, and depth, node count, text size and the
//! final encoded size are capped. That is the injection guard for `/tellraw`:
//! a hostile component cannot recurse the encoder to death or fill the
//! outbox.

use super::json_escape;
use super::nbt::Nbt;

/// Maximum nesting depth of a parsed component.
pub const MAX_DEPTH: usize = 64;
/// Maximum number of components in one tree.
pub const MAX_NODES: usize = 4096;
/// Maximum total text characters in one tree.
pub const MAX_TEXT_CHARS: usize = 32 * 1024;
/// Maximum encoded size (JSON or NBT) of one tree.
pub const MAX_BYTES: usize = 64 * 1024;

/// Which NBT spelling of the style fields a version uses.
///
/// JSON is always the old camel-case spelling; the NBT split is 1.21.5, where
/// the component codec renamed the style fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NbtForm {
    /// 1.20.3 – 1.21.4: `clickEvent`, `hoverEvent`.
    Legacy,
    /// 1.21.5+: `click_event`, `hover_event`, and the new event field names.
    Modern,
}

/// Which historical JSON spelling a component is written in.
///
/// 1.8 predates the `contents` wrapper: `show_item`/`show_entity` are a single
/// `value` SNBT string. 1.16–1.20.2 use a `contents` object. The two cannot be
/// served by one spelling, so the codec picks the form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonForm {
    /// 1.8: `show_item`/`show_entity` as a `value` SNBT string.
    Legacy8,
    /// 1.16–1.20.2: hover contents in a `contents` object.
    Standard,
}

/// A text component: content plus an inherited style and appended children.
#[derive(Clone, Debug, PartialEq)]
pub struct TextComponent {
    /// What the component says.
    pub content: Content,
    /// Formatting, interaction and display style.
    pub style: Style,
    /// Components appended after this one.
    pub extra: Vec<TextComponent>,
}

/// The content kind of a component.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    /// Literal text.
    Text(String),
    /// A translation key with optional `fallback` and arguments.
    Translate {
        /// Translation key, e.g. `chat.type.text`.
        key: String,
        /// Text shown when the client has no translation.
        fallback: Option<String>,
        /// Arguments substituted into the translation.
        with: Vec<TextComponent>,
    },
    /// A live entity selector's display names.
    Selector {
        /// The selector, e.g. `@a`.
        selector: String,
        /// Separator between multiple names.
        separator: Option<Box<TextComponent>>,
    },
    /// A scoreboard value.
    Score {
        /// Score holder name.
        name: String,
        /// Objective name.
        objective: String,
    },
    /// A key name from the client's key bindings.
    Keybind(String),
    /// A path into block/entity/storage NBT.
    Nbt {
        /// NBT path.
        path: String,
        /// Where the path is resolved.
        source: NbtSource,
        /// Whether the selected tag is itself read as a component.
        interpret: bool,
        /// Separator between multiple selected tags.
        separator: Option<Box<TextComponent>>,
    },
}

/// Where an NBT text component reads from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NbtSource {
    /// A block at a position.
    Block(String),
    /// An entity selector.
    Entity(String),
    /// A command-storage id.
    Storage(String),
    /// No source named (the component is invalid for vanilla, but kept
    /// verbatim rather than invented).
    None,
}

/// Style shared by a component and its children.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub color: Option<String>,
    pub font: Option<String>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underlined: Option<bool>,
    pub strikethrough: Option<bool>,
    pub obfuscated: Option<bool>,
    pub insertion: Option<String>,
    pub click_event: Option<ClickEvent>,
    pub hover_event: Option<HoverEvent>,
}

/// A click interaction.
#[derive(Clone, Debug, PartialEq)]
pub enum ClickEvent {
    OpenUrl(String),
    OpenFile(String),
    RunCommand(String),
    SuggestCommand(String),
    ChangePage(i32),
    CopyToClipboard(String),
    /// `show_dialog` (1.21.9+), naming a dialog by id.
    ShowDialog(String),
    Custom {
        id: String,
        payload: Option<Nbt>,
    },
}

/// A hover interaction.
#[derive(Clone, Debug, PartialEq)]
pub enum HoverEvent {
    /// Show another text component.
    Text(Box<TextComponent>),
    /// Show an item.
    Item {
        id: String,
        count: i32,
        /// Pre-1.20.5 item NBT (`tag`), kept structurally so each dialect can
        /// spell it (JSON writes SNBT; legacy NBT writes a compound).
        tag: Option<Nbt>,
        /// 1.20.5+ data components.
        components: Option<Nbt>,
    },
    /// Show an entity.
    ///
    /// `id` is the entity UUID; `kind` is its entity type. The wire spelling
    /// differs: legacy JSON/NBT writes `type` + `id`, 1.21.5+ writes `id`
    /// (type) + `uuid` (UUID).
    Entity {
        kind: String,
        id: String,
        name: Option<Box<TextComponent>>,
    },
    /// A datapack-registered custom hover.
    Custom { id: String, payload: Option<Nbt> },
}

impl TextComponent {
    /// Literal text with no style.
    pub fn text(text: impl Into<String>) -> TextComponent {
        TextComponent {
            content: Content::Text(text.into()),
            style: Style::default(),
            extra: Vec::new(),
        }
    }

    /// A translation component with arguments.
    pub fn translate(key: impl Into<String>, with: Vec<TextComponent>) -> TextComponent {
        TextComponent {
            content: Content::Translate {
                key: key.into(),
                fallback: None,
                with,
            },
            style: Style::default(),
            extra: Vec::new(),
        }
    }

    /// Parse a component from the NBT form the command argument carries.
    pub fn from_nbt(nbt: &Nbt) -> Result<TextComponent, String> {
        let mut nodes = 0usize;
        let c = parse_component(nbt, 0, &mut nodes)?;
        c.check_limits()?;
        Ok(c)
    }

    /// Check the parse-time safety limits (depth, node count, text size).
    pub fn check_limits(&self) -> Result<(), String> {
        let mut nodes = 0usize;
        let mut chars = 0usize;
        self.check_limits_at(0, &mut nodes, &mut chars)?;
        if self.to_json().len() > MAX_BYTES {
            return Err("component is too large".into());
        }
        Ok(())
    }

    fn check_limits_at(
        &self,
        depth: usize,
        nodes: &mut usize,
        chars: &mut usize,
    ) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("component is nested too deeply".into());
        }
        *nodes += 1;
        if *nodes > MAX_NODES {
            return Err("component has too many parts".into());
        }
        match &self.content {
            Content::Text(s) => *chars += s.chars().count(),
            Content::Translate {
                key,
                fallback,
                with,
            } => {
                *chars += key.chars().count();
                *chars += fallback.as_deref().map(|f| f.chars().count()).unwrap_or(0);
                for c in with {
                    c.check_limits_at(depth + 1, nodes, chars)?;
                }
            }
            Content::Selector {
                selector,
                separator,
            } => {
                *chars += selector.chars().count();
                if let Some(c) = separator {
                    c.check_limits_at(depth + 1, nodes, chars)?;
                }
            }
            Content::Score { name, objective } => {
                *chars += name.chars().count() + objective.chars().count();
            }
            Content::Keybind(k) => *chars += k.chars().count(),
            Content::Nbt {
                path, separator, ..
            } => {
                *chars += path.chars().count();
                if let Some(c) = separator {
                    c.check_limits_at(depth + 1, nodes, chars)?;
                }
            }
        }
        for c in &self.extra {
            c.check_limits_at(depth + 1, nodes, chars)?;
        }
        if let Some(hover) = &self.style.hover_event {
            match hover {
                HoverEvent::Text(c) => c.check_limits_at(depth + 1, nodes, chars)?,
                HoverEvent::Item { components, .. } => {
                    if let Some(c) = components {
                        check_nbt_limits(c, depth + 1, nodes)?;
                    }
                }
                HoverEvent::Custom { payload, .. } => {
                    if let Some(p) = payload {
                        check_nbt_limits(p, depth + 1, nodes)?;
                    }
                }
                HoverEvent::Entity { name, .. } => {
                    if let Some(n) = name {
                        n.check_limits_at(depth + 1, nodes, chars)?;
                    }
                }
            }
        }
        if let Some(ClickEvent::Custom {
            payload: Some(p), ..
        }) = &self.style.click_event
        {
            check_nbt_limits(p, depth + 1, nodes)?;
        }
        if *chars > MAX_TEXT_CHARS {
            return Err("component text is too long".into());
        }
        Ok(())
    }

    /// The standard JSON spelling (1.16–1.20.2), also what logs use.
    pub fn to_json(&self) -> String {
        self.to_json_form(JsonForm::Standard)
    }

    /// The JSON spelling for a specific client generation.
    pub fn to_json_form(&self, form: JsonForm) -> String {
        let mut fields: Vec<(String, String)> = Vec::new();
        self.content.push_json(&mut fields, form);
        self.style.push_json(&mut fields, form);
        if !self.extra.is_empty() {
            fields.push(("extra".into(), json_list(&self.extra, form)));
        }
        json_object(fields)
    }

    /// The NBT spelling used by 1.20.3+.
    pub fn to_nbt(&self, form: NbtForm) -> Nbt {
        let mut fields: Vec<(String, Nbt)> = Vec::new();
        self.content.push_nbt(&mut fields, form);
        self.style.push_nbt(&mut fields, form);
        if !self.extra.is_empty() {
            fields.push((
                "extra".into(),
                Nbt::List(self.extra.iter().map(|c| c.to_nbt(form)).collect()),
            ));
        }
        Nbt::Compound(fields)
    }

    /// A best-effort plain rendering, for logging and legacy-codec fallbacks.
    /// Translation keys are shown by their fallback, or the key itself.
    pub fn plain_text(&self) -> String {
        let mut out = match &self.content {
            Content::Text(s) => s.clone(),
            Content::Translate {
                key,
                fallback,
                with,
            } => {
                let mut s = fallback.clone().unwrap_or_else(|| key.clone());
                for c in with {
                    s.push_str(&c.plain_text());
                }
                s
            }
            Content::Selector { selector, .. } => selector.clone(),
            Content::Score { name, .. } => name.clone(),
            Content::Keybind(k) => k.clone(),
            Content::Nbt { path, .. } => path.clone(),
        };
        for c in &self.extra {
            out.push_str(&c.plain_text());
        }
        out
    }
}

/// Bound an NBT payload carried by a custom click/hover event or item
/// components. The JSON/NBT writers recurse over it, so the depth budget is
/// enforced before any writer sees it.
fn check_nbt_limits(nbt: &Nbt, depth: usize, nodes: &mut usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("component NBT is nested too deeply".into());
    }
    *nodes += 1;
    if *nodes > MAX_NODES {
        return Err("component has too many parts".into());
    }
    match nbt {
        Nbt::List(items) => {
            for item in items {
                check_nbt_limits(item, depth + 1, nodes)?;
            }
        }
        Nbt::Compound(fields) => {
            for (_, value) in fields {
                check_nbt_limits(value, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

impl Content {
    fn push_json(&self, fields: &mut Vec<(String, String)>, form: JsonForm) {
        match self {
            Content::Text(s) => fields.push(("text".into(), quote(s))),
            Content::Translate {
                key,
                fallback,
                with,
            } => {
                fields.push(("translate".into(), quote(key)));
                if let Some(f) = fallback {
                    fields.push(("fallback".into(), quote(f)));
                }
                if !with.is_empty() {
                    fields.push(("with".into(), json_list(with, form)));
                }
            }
            Content::Selector {
                selector,
                separator,
            } => {
                fields.push(("selector".into(), quote(selector)));
                if let Some(s) = separator {
                    fields.push(("separator".into(), s.to_json_form(form)));
                }
            }
            Content::Score { name, objective } => {
                let score = json_object(vec![
                    ("name".into(), quote(name)),
                    ("objective".into(), quote(objective)),
                ]);
                fields.push(("score".into(), score));
            }
            Content::Keybind(k) => fields.push(("keybind".into(), quote(k))),
            Content::Nbt {
                path,
                source,
                interpret,
                separator,
            } => {
                fields.push(("nbt".into(), quote(path)));
                match source {
                    NbtSource::Block(pos) => fields.push(("block".into(), quote(pos))),
                    NbtSource::Entity(sel) => fields.push(("entity".into(), quote(sel))),
                    NbtSource::Storage(id) => fields.push(("storage".into(), quote(id))),
                    NbtSource::None => {}
                }
                if *interpret {
                    fields.push(("interpret".into(), "true".into()));
                }
                if let Some(s) = separator {
                    fields.push(("separator".into(), s.to_json_form(form)));
                }
            }
        }
    }

    fn push_nbt(&self, fields: &mut Vec<(String, Nbt)>, form: NbtForm) {
        match self {
            Content::Text(s) => fields.push(("text".into(), Nbt::Str(s.clone()))),
            Content::Translate {
                key,
                fallback,
                with,
            } => {
                fields.push(("translate".into(), Nbt::Str(key.clone())));
                if let Some(f) = fallback {
                    fields.push(("fallback".into(), Nbt::Str(f.clone())));
                }
                if !with.is_empty() {
                    fields.push((
                        "with".into(),
                        Nbt::List(with.iter().map(|c| c.to_nbt(form)).collect()),
                    ));
                }
            }
            Content::Selector {
                selector,
                separator,
            } => {
                fields.push(("selector".into(), Nbt::Str(selector.clone())));
                if let Some(s) = separator {
                    fields.push(("separator".into(), s.to_nbt(form)));
                }
            }
            Content::Score { name, objective } => {
                fields.push((
                    "score".into(),
                    Nbt::Compound(vec![
                        ("name".into(), Nbt::Str(name.clone())),
                        ("objective".into(), Nbt::Str(objective.clone())),
                    ]),
                ));
            }
            Content::Keybind(k) => fields.push(("keybind".into(), Nbt::Str(k.clone()))),
            Content::Nbt {
                path,
                source,
                interpret,
                separator,
            } => {
                fields.push(("nbt".into(), Nbt::Str(path.clone())));
                match source {
                    NbtSource::Block(pos) => fields.push(("block".into(), Nbt::Str(pos.clone()))),
                    NbtSource::Entity(sel) => fields.push(("entity".into(), Nbt::Str(sel.clone()))),
                    NbtSource::Storage(id) => fields.push(("storage".into(), Nbt::Str(id.clone()))),
                    NbtSource::None => {}
                }
                if *interpret {
                    fields.push(("interpret".into(), Nbt::Byte(1)));
                }
                if let Some(s) = separator {
                    fields.push(("separator".into(), s.to_nbt(form)));
                }
            }
        }
    }
}

impl Style {
    fn push_json(&self, fields: &mut Vec<(String, String)>, form: JsonForm) {
        if let Some(c) = &self.color {
            fields.push(("color".into(), quote(c)));
        }
        if let Some(f) = &self.font {
            fields.push(("font".into(), quote(f)));
        }
        push_bool(fields, "bold", self.bold);
        push_bool(fields, "italic", self.italic);
        push_bool(fields, "underlined", self.underlined);
        push_bool(fields, "strikethrough", self.strikethrough);
        push_bool(fields, "obfuscated", self.obfuscated);
        if let Some(i) = &self.insertion {
            fields.push(("insertion".into(), quote(i)));
        }
        if let Some(c) = &self.click_event {
            fields.push(("clickEvent".into(), c.to_json()));
        }
        if let Some(h) = &self.hover_event {
            fields.push(("hoverEvent".into(), h.to_json(form)));
        }
    }

    fn push_nbt(&self, fields: &mut Vec<(String, Nbt)>, form: NbtForm) {
        if let Some(c) = &self.color {
            fields.push(("color".into(), Nbt::Str(c.clone())));
        }
        if let Some(f) = &self.font {
            fields.push(("font".into(), Nbt::Str(f.clone())));
        }
        for (name, value) in [
            ("bold", self.bold),
            ("italic", self.italic),
            ("underlined", self.underlined),
            ("strikethrough", self.strikethrough),
            ("obfuscated", self.obfuscated),
        ] {
            if let Some(b) = value {
                fields.push((name.into(), Nbt::Byte(b as i8)));
            }
        }
        if let Some(i) = &self.insertion {
            fields.push(("insertion".into(), Nbt::Str(i.clone())));
        }
        if let Some(c) = &self.click_event {
            let key = match form {
                NbtForm::Legacy => "clickEvent",
                NbtForm::Modern => "click_event",
            };
            fields.push((key.into(), c.to_nbt(form)));
        }
        if let Some(h) = &self.hover_event {
            let key = match form {
                NbtForm::Legacy => "hoverEvent",
                NbtForm::Modern => "hover_event",
            };
            fields.push((key.into(), h.to_nbt(form)));
        }
    }
}

impl ClickEvent {
    fn to_json(&self) -> String {
        let (action, value) = match self {
            ClickEvent::OpenUrl(v) => ("open_url", v),
            ClickEvent::OpenFile(v) => ("open_file", v),
            ClickEvent::RunCommand(v) => ("run_command", v),
            ClickEvent::SuggestCommand(v) => ("suggest_command", v),
            ClickEvent::ChangePage(v) => {
                return json_object(vec![
                    ("action".into(), quote("change_page")),
                    ("value".into(), quote(&v.to_string())),
                ]);
            }
            ClickEvent::CopyToClipboard(v) => {
                return json_object(vec![
                    ("action".into(), quote("copy_to_clipboard")),
                    ("value".into(), quote(v)),
                ]);
            }
            ClickEvent::ShowDialog(v) => {
                return json_object(vec![
                    ("action".into(), quote("show_dialog")),
                    ("dialog".into(), quote(v)),
                ]);
            }
            ClickEvent::Custom { id, payload } => {
                let mut fields = vec![("action".into(), quote("custom")), ("id".into(), quote(id))];
                if let Some(p) = payload {
                    fields.push(("payload".into(), nbt_to_json(p)));
                }
                return json_object(fields);
            }
        };
        json_object(vec![
            ("action".into(), quote(action)),
            ("value".into(), quote(value)),
        ])
    }

    fn to_nbt(&self, form: NbtForm) -> Nbt {
        let mut fields: Vec<(String, Nbt)> = Vec::new();
        match self {
            ClickEvent::OpenUrl(v) => {
                fields.push(("action".into(), Nbt::Str("open_url".into())));
                let key = match form {
                    NbtForm::Legacy => "value",
                    NbtForm::Modern => "url",
                };
                fields.push((key.into(), Nbt::Str(v.clone())));
            }
            ClickEvent::OpenFile(v) => {
                fields.push(("action".into(), Nbt::Str("open_file".into())));
                let key = match form {
                    NbtForm::Legacy => "value",
                    NbtForm::Modern => "path",
                };
                fields.push((key.into(), Nbt::Str(v.clone())));
            }
            ClickEvent::RunCommand(v) => {
                fields.push(("action".into(), Nbt::Str("run_command".into())));
                let key = match form {
                    NbtForm::Legacy => "value",
                    NbtForm::Modern => "command",
                };
                fields.push((key.into(), Nbt::Str(v.clone())));
            }
            ClickEvent::SuggestCommand(v) => {
                fields.push(("action".into(), Nbt::Str("suggest_command".into())));
                let key = match form {
                    NbtForm::Legacy => "value",
                    NbtForm::Modern => "command",
                };
                fields.push((key.into(), Nbt::Str(v.clone())));
            }
            ClickEvent::ChangePage(v) => {
                fields.push(("action".into(), Nbt::Str("change_page".into())));
                match form {
                    NbtForm::Legacy => fields.push(("value".into(), Nbt::Str(v.to_string()))),
                    NbtForm::Modern => fields.push(("page".into(), Nbt::Int(*v))),
                }
            }
            ClickEvent::CopyToClipboard(v) => {
                fields.push(("action".into(), Nbt::Str("copy_to_clipboard".into())));
                fields.push(("value".into(), Nbt::Str(v.clone())));
            }
            ClickEvent::ShowDialog(v) => {
                fields.push(("action".into(), Nbt::Str("show_dialog".into())));
                fields.push(("dialog".into(), Nbt::Str(v.clone())));
            }
            ClickEvent::Custom { id, payload } => {
                fields.push(("action".into(), Nbt::Str("custom".into())));
                fields.push(("id".into(), Nbt::Str(id.clone())));
                if let Some(p) = payload {
                    fields.push(("payload".into(), p.clone()));
                }
            }
        }
        Nbt::Compound(fields)
    }
}

impl HoverEvent {
    fn to_json(&self, form: JsonForm) -> String {
        match self {
            HoverEvent::Text(c) => {
                let key = match form {
                    JsonForm::Legacy8 => "value",
                    JsonForm::Standard => "contents",
                };
                json_object(vec![
                    ("action".into(), quote("show_text")),
                    (key.into(), c.to_json_form(form)),
                ])
            }
            HoverEvent::Item {
                id,
                count,
                tag,
                components,
            } => {
                if form == JsonForm::Legacy8 {
                    let mut snbt = format!("{{id:{},Count:{}b", quote(id), count);
                    if let Some(t) = tag {
                        snbt.push_str(&format!(",tag:{}", nbt_snbt(t)));
                    }
                    snbt.push('}');
                    return json_object(vec![
                        ("action".into(), quote("show_item")),
                        ("value".into(), quote(&snbt)),
                    ]);
                }
                let mut contents = vec![
                    ("id".into(), quote(id)),
                    ("count".into(), count.to_string()),
                ];
                if let Some(t) = tag {
                    contents.push(("tag".into(), quote(&nbt_snbt(t))));
                } else if let Some(c) = components {
                    contents.push(("components".into(), nbt_to_json(c)));
                }
                json_object(vec![
                    ("action".into(), quote("show_item")),
                    ("contents".into(), json_object(contents)),
                ])
            }
            HoverEvent::Entity { kind, id, name } => {
                if form == JsonForm::Legacy8 {
                    // 1.8 carries the entity as one SNBT string.
                    let mut snbt = format!("{{type:{},id:{}", quote(kind), quote(id));
                    if let Some(n) = name {
                        snbt.push_str(&format!(",name:{}", quote(&n.plain_text())));
                    }
                    snbt.push('}');
                    return json_object(vec![
                        ("action".into(), quote("show_entity")),
                        ("value".into(), quote(&snbt)),
                    ]);
                }
                let mut contents = vec![("type".into(), quote(kind)), ("id".into(), quote(id))];
                if let Some(n) = name {
                    contents.push(("name".into(), n.to_json_form(form)));
                }
                json_object(vec![
                    ("action".into(), quote("show_entity")),
                    ("contents".into(), json_object(contents)),
                ])
            }
            HoverEvent::Custom { id, payload } => {
                let mut fields = vec![("action".into(), quote("custom")), ("id".into(), quote(id))];
                if let Some(p) = payload {
                    fields.push(("payload".into(), nbt_to_json(p)));
                }
                json_object(fields)
            }
        }
    }

    fn to_nbt(&self, form: NbtForm) -> Nbt {
        let mut fields: Vec<(String, Nbt)> = Vec::new();
        match self {
            HoverEvent::Text(c) => {
                fields.push(("action".into(), Nbt::Str("show_text".into())));
                let key = match form {
                    NbtForm::Legacy => "contents",
                    NbtForm::Modern => "value",
                };
                fields.push((key.into(), c.to_nbt(form)));
            }
            HoverEvent::Item {
                id,
                count,
                tag,
                components,
            } => {
                fields.push(("action".into(), Nbt::Str("show_item".into())));
                match form {
                    // 1.16–1.21.4 wraps the item in `contents`.
                    NbtForm::Legacy => {
                        let mut contents = vec![
                            ("id".into(), Nbt::Str(id.clone())),
                            ("count".into(), Nbt::Int(*count)),
                        ];
                        if let Some(t) = tag {
                            contents.push(("tag".into(), t.clone()));
                        } else if let Some(c) = components {
                            contents.push(("components".into(), c.clone()));
                        }
                        fields.push(("contents".into(), Nbt::Compound(contents)));
                    }
                    // 1.21.5+ flattens the fields.
                    NbtForm::Modern => {
                        fields.push(("id".into(), Nbt::Str(id.clone())));
                        fields.push(("count".into(), Nbt::Int(*count)));
                        if let Some(c) = components {
                            fields.push(("components".into(), c.clone()));
                        }
                    }
                }
            }
            HoverEvent::Entity { kind, id, name } => {
                fields.push(("action".into(), Nbt::Str("show_entity".into())));
                match form {
                    // Legacy NBT (1.20.3–1.21.4): `type` names the entity
                    // type and `id` is the UUID, wrapped in `contents`.
                    NbtForm::Legacy => {
                        let mut contents = vec![
                            ("type".into(), Nbt::Str(kind.clone())),
                            ("id".into(), Nbt::Str(id.clone())),
                        ];
                        if let Some(n) = name {
                            contents.push(("name".into(), n.to_nbt(form)));
                        }
                        fields.push(("contents".into(), Nbt::Compound(contents)));
                    }
                    // 1.21.5 renamed the fields: `id` is the entity type and
                    // `uuid` is the UUID.
                    NbtForm::Modern => {
                        fields.push(("id".into(), Nbt::Str(kind.clone())));
                        fields.push(("uuid".into(), Nbt::Str(id.clone())));
                        if let Some(n) = name {
                            fields.push(("name".into(), n.to_nbt(form)));
                        }
                    }
                }
            }
            HoverEvent::Custom { id, payload } => {
                fields.push(("action".into(), Nbt::Str("custom".into())));
                fields.push(("id".into(), Nbt::Str(id.clone())));
                if let Some(p) = payload {
                    fields.push(("payload".into(), p.clone()));
                }
            }
        }
        Nbt::Compound(fields)
    }
}

fn parse_component(nbt: &Nbt, depth: usize, nodes: &mut usize) -> Result<TextComponent, String> {
    if depth > MAX_DEPTH {
        return Err("component is nested too deeply".into());
    }
    *nodes += 1;
    if *nodes > MAX_NODES {
        return Err("component has too many parts".into());
    }
    match nbt {
        Nbt::Str(s) => Ok(TextComponent::text(s.clone())),
        Nbt::List(items) => {
            // A bare list is an array of components: an empty root plus extras.
            let mut extra = Vec::with_capacity(items.len());
            for item in items {
                extra.push(parse_component(item, depth + 1, nodes)?);
            }
            Ok(TextComponent {
                content: Content::Text(String::new()),
                style: Style::default(),
                extra,
            })
        }
        Nbt::Compound(fields) => {
            let content = if let Some(v) = field(fields, "text") {
                Content::Text(as_str(v, "text")?.to_string())
            } else if let Some(v) = field(fields, "translate") {
                let key = as_str(v, "translate")?.to_string();
                validate_key(&key)?;
                let fallback = match field(fields, "fallback") {
                    Some(v) => Some(as_str(v, "fallback")?.to_string()),
                    None => None,
                };
                let with = match field(fields, "with") {
                    Some(v) => parse_component_list(v, depth, nodes)?,
                    None => Vec::new(),
                };
                Content::Translate {
                    key,
                    fallback,
                    with,
                }
            } else if let Some(v) = field(fields, "selector") {
                let selector = as_str(v, "selector")?.to_string();
                validate_plain_value(&selector, "selector")?;
                let separator = match field(fields, "separator") {
                    Some(v) => Some(Box::new(parse_component(v, depth + 1, nodes)?)),
                    None => None,
                };
                Content::Selector {
                    selector,
                    separator,
                }
            } else if let Some(v) = field(fields, "score") {
                let score = as_compound(v, "score")?;
                let name = get_str(score, "name")?;
                let objective = get_str(score, "objective")?;
                validate_plain_value(&name, "score holder")?;
                validate_plain_value(&objective, "objective")?;
                Content::Score { name, objective }
            } else if let Some(v) = field(fields, "keybind") {
                let key = as_str(v, "keybind")?.to_string();
                validate_resource_location(&key, "keybind")?;
                Content::Keybind(key)
            } else if let Some(v) = field(fields, "nbt") {
                let path = as_str(v, "nbt")?.to_string();
                validate_plain_value(&path, "nbt path")?;
                let source = if let Some(v) = field(fields, "block") {
                    NbtSource::Block(as_str(v, "block")?.to_string())
                } else if let Some(v) = field(fields, "entity") {
                    NbtSource::Entity(as_str(v, "entity")?.to_string())
                } else if let Some(v) = field(fields, "storage") {
                    NbtSource::Storage(as_str(v, "storage")?.to_string())
                } else {
                    NbtSource::None
                };
                let interpret = match field(fields, "interpret") {
                    Some(v) => as_bool(v, "interpret")?,
                    None => false,
                };
                let separator = match field(fields, "separator") {
                    Some(v) => Some(Box::new(parse_component(v, depth + 1, nodes)?)),
                    None => None,
                };
                Content::Nbt {
                    path,
                    source,
                    interpret,
                    separator,
                }
            } else if field(fields, "extra").is_some() {
                // A style-only component with children is accepted; vanilla
                // treats it as an empty text component.
                Content::Text(String::new())
            } else {
                return Err("component has no recognised content".into());
            };
            let style = parse_style(fields, depth, nodes)?;
            let extra = match field(fields, "extra") {
                Some(v) => parse_component_list(v, depth, nodes)?,
                None => Vec::new(),
            };
            Ok(TextComponent {
                content,
                style,
                extra,
            })
        }
        _ => Err("a text component must be a string, a list or a compound".into()),
    }
}

fn parse_component_list(
    nbt: &Nbt,
    depth: usize,
    nodes: &mut usize,
) -> Result<Vec<TextComponent>, String> {
    let items = match nbt {
        Nbt::List(items) => items,
        // Tolerate a single component where a list is expected, as the
        // pre-1.20 JSON parser did.
        other => std::slice::from_ref(other),
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(parse_component(item, depth + 1, nodes)?);
    }
    Ok(out)
}

fn parse_style(fields: &[(String, Nbt)], depth: usize, nodes: &mut usize) -> Result<Style, String> {
    let mut style = Style::default();
    if let Some(v) = field(fields, "color") {
        let color = as_str(v, "color")?;
        validate_color(color)?;
        style.color = Some(color.to_string());
    }
    if let Some(v) = field(fields, "font") {
        let font = as_str(v, "font")?;
        validate_resource_location(font, "font")?;
        style.font = Some(font.to_string());
    }
    style.bold = parse_optional_bool(fields, "bold")?;
    style.italic = parse_optional_bool(fields, "italic")?;
    style.underlined = parse_optional_bool(fields, "underlined")?;
    style.strikethrough = parse_optional_bool(fields, "strikethrough")?;
    style.obfuscated = parse_optional_bool(fields, "obfuscated")?;
    if let Some(v) = field(fields, "insertion") {
        let insertion = as_str(v, "insertion")?;
        validate_plain_value(insertion, "insertion")?;
        style.insertion = Some(insertion.to_string());
    }
    if let Some(v) = field(fields, "clickEvent").or_else(|| field(fields, "click_event")) {
        style.click_event = Some(parse_click(v)?);
    }
    if let Some(v) = field(fields, "hoverEvent").or_else(|| field(fields, "hover_event")) {
        style.hover_event = Some(parse_hover(v, depth, nodes)?);
    }
    Ok(style)
}

fn parse_click(nbt: &Nbt) -> Result<ClickEvent, String> {
    let fields = as_compound(nbt, "click event")?;
    let action = get_str(fields, "action")?;
    let value = |primary: &str| -> Result<String, String> {
        if let Some(v) = field(fields, primary) {
            Ok(as_str(v, primary)?.to_string())
        } else if let Some(v) = field(fields, "value") {
            Ok(as_str(v, "value")?.to_string())
        } else {
            Err(format!("click event '{}' has no value", action))
        }
    };
    Ok(match action.as_str() {
        "open_url" => {
            let url = value("url")?;
            validate_url(&url)?;
            ClickEvent::OpenUrl(url)
        }
        "open_file" => {
            let path = value("path")?;
            validate_plain_value(&path, "open_file path")?;
            ClickEvent::OpenFile(path)
        }
        "run_command" => {
            let command = value("command")?;
            validate_command(&command)?;
            ClickEvent::RunCommand(command)
        }
        "suggest_command" => {
            let command = value("command")?;
            validate_command(&command)?;
            ClickEvent::SuggestCommand(command)
        }
        "change_page" => {
            let page = if let Some(v) = field(fields, "page") {
                as_i32(v, "page")?
            } else if let Some(v) = field(fields, "value") {
                as_i32(v, "value")?
            } else {
                return Err("click event 'change_page' has no page".into());
            };
            ClickEvent::ChangePage(page)
        }
        "copy_to_clipboard" => {
            let text = value("value")?;
            validate_plain_value(&text, "clipboard text")?;
            ClickEvent::CopyToClipboard(text)
        }
        "show_dialog" => {
            let id = if let Some(v) = field(fields, "dialog").or_else(|| field(fields, "value")) {
                as_str(v, "dialog")?.to_string()
            } else {
                return Err("click event 'show_dialog' has no dialog".into());
            };
            validate_resource_location(&id, "dialog")?;
            ClickEvent::ShowDialog(id)
        }
        "custom" => {
            let id = get_str(fields, "id")?;
            validate_resource_location(&id, "custom click id")?;
            let payload = field(fields, "payload").cloned();
            ClickEvent::Custom { id, payload }
        }
        other => return Err(format!("unknown click event action '{other}'")),
    })
}

fn parse_hover(nbt: &Nbt, depth: usize, nodes: &mut usize) -> Result<HoverEvent, String> {
    let fields = as_compound(nbt, "hover event")?;
    let action = get_str(fields, "action")?;
    Ok(match action.as_str() {
        "show_text" => {
            let v = field(fields, "value")
                .or_else(|| field(fields, "contents"))
                .ok_or_else(|| "hover event 'show_text' has no value".to_string())?;
            HoverEvent::Text(Box::new(parse_component(v, depth + 1, nodes)?))
        }
        "show_item" => {
            // 1.16–1.21.4 wrap the item in `contents`; 1.8 uses a single
            // `value` SNBT string. 1.21.5+ flattens the fields.
            let inner = hover_contents(fields, "show_item")?;
            let id = get_str(&inner, "id")?;
            validate_resource_location(&id, "item id")?;
            let count = match field(&inner, "count") {
                Some(v) => as_i32(v, "count")?,
                None => 1,
            };
            if !(1..=99).contains(&count) {
                return Err("hover event 'show_item' count must be 1..=99".into());
            }
            let tag = match field(&inner, "tag") {
                Some(Nbt::Str(s)) => Some(
                    crate::commands::snbt::parse(s)
                        .map_err(|e| format!("invalid show_item tag: {e}"))?,
                ),
                Some(other) => Some(other.clone()),
                None => None,
            };
            let components = field(&inner, "components").cloned();
            HoverEvent::Item {
                id,
                count,
                tag,
                components,
            }
        }
        "show_entity" => {
            let inner = hover_contents(fields, "show_entity")?;
            // 1.21.5+ spells the type `id` and the UUID `uuid`; the older
            // dialects spell them `type` and `id`. Accept both so a component
            // copied from either documentation parses.
            let (kind, id) = if field(&inner, "uuid").is_some() {
                let kind = get_str(&inner, "id")?;
                let uuid = normalize_uuid(&get_str(&inner, "uuid")?)?;
                validate_resource_location(&kind, "entity type")?;
                (kind, uuid)
            } else {
                let kind = match field(&inner, "type") {
                    Some(v) => as_str(v, "type")?.to_string(),
                    // Vanilla's pre-1.21.5 codec defaults a missing type to a
                    // pig rather than refusing the component.
                    None => "minecraft:pig".to_string(),
                };
                let id = normalize_uuid(&get_str(&inner, "id")?)?;
                validate_resource_location(&kind, "entity type")?;
                (kind, id)
            };
            let name = match field(&inner, "name") {
                Some(v) => Some(Box::new(parse_component(v, depth + 1, nodes)?)),
                None => None,
            };
            HoverEvent::Entity { kind, id, name }
        }
        "custom" => {
            let id = get_str(fields, "id")?;
            validate_resource_location(&id, "custom hover id")?;
            let payload = field(fields, "payload").cloned();
            HoverEvent::Custom { id, payload }
        }
        other => return Err(format!("unknown hover event action '{other}'")),
    })
}

/// The effective fields of a hover event: a `contents` compound, a pre-1.16
/// `value` SNBT string, or the event's own fields (1.21.5+).
fn hover_contents(fields: &[(String, Nbt)], what: &str) -> Result<Vec<(String, Nbt)>, String> {
    if let Some(v) = field(fields, "contents") {
        return Ok(as_compound(v, what)?.to_vec());
    }
    if let Some(Nbt::Str(s)) = field(fields, "value") {
        let parsed =
            crate::commands::snbt::parse(s).map_err(|e| format!("invalid {what} value: {e}"))?;
        return Ok(as_compound(&parsed, what)?.to_vec());
    }
    Ok(fields.to_vec())
}

/// Normalise a UUID to the hyphenated 8-4-4-4-12 form the client codec emits
/// and expects.
fn normalize_uuid(uuid: &str) -> Result<String, String> {
    validate_uuid(uuid)?;
    let hex: String = uuid.chars().filter(|c| *c != '-').collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// The sixteen named colours a [`TextColor`] accepts, plus a `#RRGGBB` hex
/// value. Anything else is a value the client codec rejects.
fn validate_color(color: &str) -> Result<(), String> {
    const NAMED: &[&str] = &[
        "black",
        "dark_blue",
        "dark_green",
        "dark_aqua",
        "dark_red",
        "dark_purple",
        "gold",
        "gray",
        "dark_gray",
        "blue",
        "green",
        "aqua",
        "red",
        "light_purple",
        "yellow",
        "white",
    ];
    if NAMED.contains(&color) {
        return Ok(());
    }
    if let Some(hex) = color.strip_prefix('#') {
        if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(());
        }
    }
    Err(format!("invalid colour '{color}'"))
}

/// A resource location (`namespace:path`, or a bare path). The client's
/// codecs reject anything outside the allowed character set.
fn validate_resource_location(value: &str, what: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{what} must not be empty"));
    }
    let (namespace, path) = match value.split_once(':') {
        Some((ns, path)) => (ns, path),
        None => ("minecraft", value),
    };
    let valid_ns = !namespace.is_empty()
        && namespace
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'));
    let valid_path = !path.is_empty()
        && path.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '/')
        });
    if valid_ns && valid_path {
        Ok(())
    } else {
        Err(format!("invalid {what} '{value}'"))
    }
}

/// A string a client interpolates directly: no control characters, and a
/// bounded length.
fn validate_plain_value(value: &str, what: &str) -> Result<(), String> {
    if value.chars().any(char::is_control) {
        return Err(format!("{what} must not contain control characters"));
    }
    if value.chars().count() > 32_767 {
        return Err(format!("{what} is too long"));
    }
    Ok(())
}

/// `open_url` accepts only an `http`/`https` URI, as the client's click-event
/// codec does.
fn validate_url(url: &str) -> Result<(), String> {
    validate_plain_value(url, "url")?;
    let lower = url.to_ascii_lowercase();
    if (lower.starts_with("http://") || lower.starts_with("https://"))
        && !url.chars().any(char::is_whitespace)
    {
        Ok(())
    } else {
        Err(format!("invalid url '{url}'"))
    }
}

/// A command carried by a click event: no control characters, non-empty and
/// bounded.
fn validate_command(command: &str) -> Result<(), String> {
    if command.is_empty() {
        return Err("command must not be empty".into());
    }
    validate_plain_value(command, "command")
}

/// A UUID in either the hyphenated or the compact form.
fn validate_uuid(uuid: &str) -> Result<(), String> {
    let hex: String = uuid.chars().filter(|c| *c != '-').collect();
    if hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(format!("invalid uuid '{uuid}'"))
    }
}

/// A translation key: a non-empty dotted identifier, as the client's language
/// table uses.
fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("translation key must not be empty".into());
    }
    validate_plain_value(key, "translation key")
}

fn field<'a>(fields: &'a [(String, Nbt)], key: &str) -> Option<&'a Nbt> {
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn get_str(fields: &[(String, Nbt)], key: &str) -> Result<String, String> {
    match field(fields, key) {
        Some(v) => Ok(as_str(v, key)?.to_string()),
        None => Err(format!("missing '{key}'")),
    }
}

fn as_str<'a>(nbt: &'a Nbt, key: &str) -> Result<&'a str, String> {
    match nbt {
        Nbt::Str(s) => Ok(s),
        _ => Err(format!("'{key}' must be a string")),
    }
}

fn as_compound<'a>(nbt: &'a Nbt, key: &str) -> Result<&'a [(String, Nbt)], String> {
    match nbt {
        Nbt::Compound(fields) => Ok(fields),
        _ => Err(format!("'{key}' must be a compound")),
    }
}

fn as_bool(nbt: &Nbt, key: &str) -> Result<bool, String> {
    match nbt {
        Nbt::Byte(v) => Ok(*v != 0),
        Nbt::Short(v) => Ok(*v != 0),
        Nbt::Int(v) => Ok(*v != 0),
        Nbt::Long(v) => Ok(*v != 0),
        _ => Err(format!("'{key}' must be a boolean")),
    }
}

fn as_i32(nbt: &Nbt, key: &str) -> Result<i32, String> {
    match nbt {
        Nbt::Byte(v) => Ok(*v as i32),
        Nbt::Short(v) => Ok(*v as i32),
        Nbt::Int(v) => Ok(*v),
        Nbt::Long(v) => Ok(*v as i32),
        Nbt::Str(s) => s
            .parse::<i32>()
            .map_err(|_| format!("'{key}' must be an integer")),
        _ => Err(format!("'{key}' must be an integer")),
    }
}

fn parse_optional_bool(fields: &[(String, Nbt)], key: &str) -> Result<Option<bool>, String> {
    match field(fields, key) {
        Some(v) => Ok(Some(as_bool(v, key)?)),
        None => Ok(None),
    }
}

fn push_bool(fields: &mut Vec<(String, String)>, key: &str, value: Option<bool>) {
    if let Some(v) = value {
        fields.push((key.into(), if v { "true" } else { "false" }.into()));
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", json_escape(s))
}

fn json_object(fields: Vec<(String, String)>) -> String {
    let body = fields
        .into_iter()
        .map(|(k, v)| format!("{}:{}", quote(&k), v))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{body}}}")
}

fn json_list(items: &[TextComponent], form: JsonForm) -> String {
    let body = items
        .iter()
        .map(|c| c.to_json_form(form))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{body}]")
}

/// A general NBT-to-JSON renderer, used for custom component payloads.
pub fn nbt_to_json(nbt: &Nbt) -> String {
    match nbt {
        Nbt::Byte(v) => v.to_string(),
        Nbt::Short(v) => v.to_string(),
        Nbt::Int(v) => v.to_string(),
        Nbt::Long(v) => v.to_string(),
        Nbt::Float(v) => v.to_string(),
        Nbt::Double(v) => v.to_string(),
        Nbt::ByteArray(v) => json_numbers(v.iter().map(|b| *b as i64)),
        Nbt::Str(s) => quote(s),
        Nbt::List(items) => {
            let body = items.iter().map(nbt_to_json).collect::<Vec<_>>().join(",");
            format!("[{body}]")
        }
        Nbt::Compound(fields) => json_object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), nbt_to_json(v)))
                .collect(),
        ),
        Nbt::IntArray(v) => json_numbers(v.iter().map(|i| *i as i64)),
        Nbt::LongArray(v) => json_numbers(v.iter().copied()),
    }
}

/// Render an NBT value as SNBT, the form the old item/entity hover payloads
/// and vanilla's non-interpreting NBT component use.
pub fn nbt_snbt(nbt: &Nbt) -> String {
    match nbt {
        Nbt::Byte(v) => format!("{v}b"),
        Nbt::Short(v) => format!("{v}s"),
        Nbt::Int(v) => v.to_string(),
        Nbt::Long(v) => format!("{v}L"),
        Nbt::Float(v) => format!("{v}f"),
        Nbt::Double(v) => format!("{v}d"),
        Nbt::ByteArray(v) => {
            let body = v
                .iter()
                .map(|b| format!("{b}b"))
                .collect::<Vec<_>>()
                .join(",");
            format!("[B;{body}]")
        }
        Nbt::Str(s) => quote(s),
        Nbt::List(items) => {
            let body = items.iter().map(nbt_snbt).collect::<Vec<_>>().join(",");
            format!("[{body}]")
        }
        Nbt::Compound(fields) => {
            let body = fields
                .iter()
                .map(|(k, v)| {
                    let key = if k
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
                    {
                        k.clone()
                    } else {
                        quote(k)
                    };
                    format!("{key}:{}", nbt_snbt(v))
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        Nbt::IntArray(v) => {
            let body = v
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("[I;{body}]")
        }
        Nbt::LongArray(v) => {
            let body = v
                .iter()
                .map(|i| format!("{i}L"))
                .collect::<Vec<_>>()
                .join(",");
            format!("[L;{body}]")
        }
    }
}

fn json_numbers(items: impl Iterator<Item = i64>) -> String {
    let body = items.map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    format!("[{body}]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::snbt;

    fn parse(text: &str) -> Result<TextComponent, String> {
        let nbt = snbt::parse(text).map_err(|e| e.to_string())?;
        TextComponent::from_nbt(&nbt)
    }

    #[test]
    fn plain_and_styled_text_round_trip() {
        let c = parse(r#"{text:"hello",color:"red",bold:true,italic:false}"#).unwrap();
        assert_eq!(c.content, Content::Text("hello".into()));
        assert_eq!(c.style.color.as_deref(), Some("red"));
        assert_eq!(c.style.bold, Some(true));
        assert_eq!(c.style.italic, Some(false));
        assert_eq!(
            c.to_json(),
            r#"{"text":"hello","color":"red","bold":true,"italic":false}"#
        );
        let nbt = c.to_nbt(NbtForm::Modern);
        assert!(matches!(nbt, Nbt::Compound(_)));
    }

    #[test]
    fn translate_fallback_with_and_extra_parse() {
        let c = parse(
            r#"{translate:"chat.type.text",fallback:"%s says %s",with:["Bob","hi"],extra:[{text:"!",bold:true}]}"#,
        )
        .unwrap();
        match &c.content {
            Content::Translate {
                key,
                fallback,
                with,
            } => {
                assert_eq!(key, "chat.type.text");
                assert_eq!(fallback.as_deref(), Some("%s says %s"));
                assert_eq!(with.len(), 2);
                assert_eq!(with[0].plain_text(), "Bob");
            }
            other => panic!("wrong content: {other:?}"),
        }
        assert_eq!(c.extra.len(), 1);
        let json = c.to_json();
        assert!(json.contains(r#""translate":"chat.type.text""#));
        assert!(json.contains(r#""with":[{"text":"Bob"},{"text":"hi"}]"#));
        assert!(json.contains(r#""extra":[{"text":"!","bold":true}]"#));
    }

    #[test]
    fn selector_score_keybind_and_nbt_parse() {
        for (text, check) in [
            (r#"{selector:"@a",separator:{text:", "}}"#, "selector"),
            (r#"{score:{name:"Bob",objective:"kills"}}"#, "score"),
            (r#"{keybind:"key.jump"}"#, "keybind"),
            (
                r#"{nbt:"foo.bar",block:"1 2 3",interpret:true,separator:{text:"|"}}"#,
                "nbt",
            ),
        ] {
            let c = parse(text).unwrap();
            assert!(c.to_json().contains(&format!("\"{check}\"")));
            assert!(matches!(c.to_nbt(NbtForm::Modern), Nbt::Compound(_)));
        }
    }

    #[test]
    fn click_and_hover_events_accept_both_spellings_and_forms() {
        let old = parse(
            r#"{text:"x",clickEvent:{action:"run_command",value:"/say hi"},hoverEvent:{action:"show_text",contents:"tip"},insertion:"ins"}"#,
        )
        .unwrap();
        assert_eq!(
            old.style.click_event,
            Some(ClickEvent::RunCommand("/say hi".into()))
        );
        assert!(matches!(old.style.hover_event, Some(HoverEvent::Text(_))));
        assert_eq!(old.style.insertion.as_deref(), Some("ins"));

        let new = parse(
            r#"{text:"x",click_event:{action:"open_url",url:"https://example.com"},hover_event:{action:"show_text",value:{text:"tip"}}}"#,
        )
        .unwrap();
        assert_eq!(
            new.style.click_event,
            Some(ClickEvent::OpenUrl("https://example.com".into()))
        );
        let modern = new.to_nbt(NbtForm::Modern);
        let bytes = modern.to_network();
        assert!(bytes.windows(11).any(|w| w == b"click_event"));
        assert!(bytes.windows(11).any(|w| w == b"hover_event"));
        assert!(!new.to_json().contains("click_event"));
        assert!(new.to_json().contains("clickEvent"));
    }

    #[test]
    fn a_bare_string_and_a_list_of_components_parse() {
        let c = parse("\"hello\"").unwrap();
        assert_eq!(c.content, Content::Text("hello".into()));
        let c = parse(r#"[{text:"a"},{text:"b",bold:true}]"#).unwrap();
        assert_eq!(c.extra.len(), 2);
        assert_eq!(c.extra[0].plain_text(), "a");
    }

    #[test]
    fn malformed_components_are_errors_not_panics() {
        for bad in [
            "{text:1}",
            "{color:1}",
            "{text:\"x\",bold:1.5}",
            "{score:{name:\"x\"}}",
            "{translate:\"x\",with:1}",
            "{clickEvent:{action:\"run_command\"}}",
            "{clickEvent:{action:\"not_an_action\",value:\"x\"}}",
            "{hoverEvent:{action:\"show_nothing\"}}",
            "{nothing:1}",
            "1b",
            "[1b,2b]",
        ] {
            assert!(parse(bad).is_err(), "`{bad}` should not parse");
        }
    }

    #[test]
    fn depth_and_size_limits_stop_a_hostile_component() {
        let mut text = "hi".to_string();
        for _ in 0..(MAX_DEPTH + 2) {
            text = format!(r#"{{text:"x",extra:[{text}]}}"#);
        }
        assert!(parse(&text).is_err(), "too deep must be refused");

        let huge = format!("{{text:\"{}\"}}", "a".repeat(MAX_TEXT_CHARS + 1));
        assert!(parse(&huge).is_err(), "too long must be refused");
    }

    #[test]
    fn json_escaping_is_correct_for_quotes_and_control_characters() {
        let c = TextComponent::text("a\"b\\c\n");
        let json = c.to_json();
        assert_eq!(json, r#"{"text":"a\"b\\c\n"}"#);
    }

    #[test]
    fn the_modern_nbt_uses_snake_case_style_keys() {
        let c = parse(
            r#"{text:"x",clickEvent:{action:"open_file",value:"/tmp/a"},hoverEvent:{action:"show_text",value:"t"}}"#,
        )
        .unwrap();
        let modern = c.to_nbt(NbtForm::Modern);
        let json = nbt_to_json(&modern);
        assert!(json.contains(r#""click_event":{"action":"open_file","path":"/tmp/a"}"#));
        assert!(json.contains(r#""hover_event":{"action":"show_text","value":{"text":"t"}}"#));

        let legacy = c.to_nbt(NbtForm::Legacy);
        let json = nbt_to_json(&legacy);
        assert!(json.contains(r#""clickEvent":{"action":"open_file","value":"/tmp/a"}"#));
        assert!(json.contains(r#""hoverEvent":{"action":"show_text","contents":{"text":"t"}}"#));
    }

    #[test]
    fn modern_show_entity_uses_id_for_type_and_uuid_for_the_uuid() {
        let uuid = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
        let c = parse(&format!(
            r#"{{text:"x",hover_event:{{action:"show_entity",id:"minecraft:pig",uuid:"{uuid}",name:"Pig"}}}}"#
        ))
        .unwrap();
        assert_eq!(
            c.style.hover_event,
            Some(HoverEvent::Entity {
                kind: "minecraft:pig".into(),
                id: uuid.into(),
                name: Some(Box::new(TextComponent::text("Pig"))),
            })
        );
        let modern = nbt_to_json(&c.to_nbt(NbtForm::Modern));
        assert!(
            modern.contains(&format!(
                r#""hover_event":{{"action":"show_entity","id":"minecraft:pig","uuid":"{uuid}""#
            )),
            "{modern}"
        );
        assert!(!modern.contains(r#""type":"minecraft:pig""#), "{modern}");

        // The legacy dialects keep `type` + `id`, and the parser accepts a
        // component copied from one of them.
        let legacy = nbt_to_json(&c.to_nbt(NbtForm::Legacy));
        assert!(
            legacy.contains(&format!(
                r#""hoverEvent":{{"action":"show_entity","contents":{{"type":"minecraft:pig","id":"{uuid}""#
            )),
            "{legacy}"
        );
        let back = parse(&format!(
            r#"{{text:"x",hoverEvent:{{action:"show_entity",type:"minecraft:pig",id:"{uuid}",name:"Pig"}}}}"#
        ))
        .unwrap();
        assert_eq!(back.style.hover_event, c.style.hover_event);
    }

    #[test]
    fn json_show_text_uses_value_for_1_8_and_contents_for_1_16() {
        let c = parse(r#"{text:"x",hover_event:{action:"show_text",value:"tip"}}"#).unwrap();
        let legacy = c.to_json_form(JsonForm::Legacy8);
        assert!(
            legacy.contains(r#""hoverEvent":{"action":"show_text","value":{"text":"tip"}}"#),
            "{legacy}"
        );
        assert!(!legacy.contains("contents"), "{legacy}");

        let standard = c.to_json();
        assert!(
            standard.contains(r#""hoverEvent":{"action":"show_text","contents":{"text":"tip"}}"#),
            "{standard}"
        );
    }

    #[test]
    fn legacy_hover_wrappers_match_each_client_generation() {
        let item =
            parse(r#"{text:"x",hoverEvent:{action:"show_item",id:"minecraft:stone",count:1}}"#)
                .unwrap();
        let v8 = item.to_json_form(JsonForm::Legacy8);
        assert!(
            v8.contains(r#""value":"{id:\"minecraft:stone\",Count:1b}""#),
            "{v8}"
        );
        let json = item.to_json();
        assert!(
            json.contains(r#""contents":{"id":"minecraft:stone","count":1}"#),
            "{json}"
        );
        let legacy_nbt = nbt_to_json(&item.to_nbt(NbtForm::Legacy));
        assert!(
            legacy_nbt.contains(r#""contents":{"id":"minecraft:stone","count":1}"#),
            "{legacy_nbt}"
        );
        let modern_nbt = nbt_to_json(&item.to_nbt(NbtForm::Modern));
        assert!(
            modern_nbt.contains(r#""id":"minecraft:stone","count":1"#),
            "{modern_nbt}"
        );
        assert!(!modern_nbt.contains("contents"), "{modern_nbt}");

        let uuid = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
        let entity = parse(&format!(
            r#"{{text:"x",hoverEvent:{{action:"show_entity",type:"minecraft:pig",id:"{uuid}"}}}}"#
        ))
        .unwrap();
        let v8 = entity.to_json_form(JsonForm::Legacy8);
        assert!(
            v8.contains(&format!(
                r#""value":"{{type:\"minecraft:pig\",id:\"{uuid}\"}}""#
            )),
            "{v8}"
        );
        let json = entity.to_json();
        assert!(
            json.contains(&format!(
                r#""contents":{{"type":"minecraft:pig","id":"{uuid}"}}"#
            )),
            "{json}"
        );
        let legacy_nbt = nbt_to_json(&entity.to_nbt(NbtForm::Legacy));
        assert!(
            legacy_nbt.contains(&format!(
                r#""contents":{{"type":"minecraft:pig","id":"{uuid}"}}"#
            )),
            "{legacy_nbt}"
        );
    }

    #[test]
    fn a_compact_uuid_is_written_hyphenated() {
        let c = parse(
            r#"{text:"x",hover_event:{action:"show_entity",id:"minecraft:pig",uuid:"069a79f444e94726a5befca90e38aaf5"}}"#,
        )
        .unwrap();
        let json = nbt_to_json(&c.to_nbt(NbtForm::Modern));
        assert!(
            json.contains("069a79f4-44e9-4726-a5be-fca90e38aaf5"),
            "{json}"
        );
    }

    #[test]
    fn values_the_client_would_reject_are_refused_at_parse_time() {
        for bad in [
            r#"{text:"x",color:"bogus"}"#,
            r##"{text:"x",color:"#12345"}"##,
            r#"{text:"x",font:"Not A Font"}"#,
            r#"{text:"x",clickEvent:{action:"open_url",value:"file:///etc/passwd"}}"#,
            r#"{text:"x",clickEvent:{action:"open_url",value:"javascript:alert(1)"}}"#,
            r#"{text:"x",clickEvent:{action:"run_command",value:"a\u0000b"}}"#,
            r#"{text:"x",hoverEvent:{action:"show_item",id:"Stone",count:1}}"#,
            r#"{text:"x",hoverEvent:{action:"show_entity",type:"minecraft:pig",id:"not-a-uuid"}}"#,
        ] {
            assert!(parse(bad).is_err(), "`{bad}` should not parse");
        }
        // The accepted forms stay accepted.
        for good in [
            r##"{text:"x",color:"#a0B1c2"}"##,
            r#"{text:"x",font:"minecraft:default"}"#,
            r#"{text:"x",clickEvent:{action:"open_url",value:"https://example.com/a?b=c"}}"#,
            r#"{text:"x",clickEvent:{action:"run_command",value:"/say hi"}}"#,
            r#"{text:"x",clickEvent:{action:"copy_to_clipboard",value:"hello"}}"#,
            r#"{text:"x",clickEvent:{action:"show_dialog",dialog:"minecraft:server_links"}}"#,
        ] {
            assert!(
                parse(good).is_ok(),
                "`{good}` should parse: {:?}",
                parse(good).err()
            );
        }
    }

    #[test]
    fn check_limits_rejects_a_deep_payload_that_bypassed_snbt() {
        let mut payload = Nbt::Byte(1);
        for _ in 0..(MAX_DEPTH + 2) {
            payload = Nbt::Compound(vec![("x".into(), payload)]);
        }
        let component = TextComponent {
            content: Content::Text("x".into()),
            style: Style {
                click_event: Some(ClickEvent::Custom {
                    id: "minecraft:test".into(),
                    payload: Some(payload.clone()),
                }),
                hover_event: Some(HoverEvent::Item {
                    id: "minecraft:stone".into(),
                    count: 1,
                    tag: None,
                    components: Some(payload),
                }),
                ..Style::default()
            },
            extra: Vec::new(),
        };
        assert!(component.check_limits().is_err());
    }

    #[test]
    fn deep_nbt_payloads_are_bounded_too() {
        // A custom click payload nested beyond the component depth budget.
        let mut payload = "1b".to_string();
        for _ in 0..(MAX_DEPTH + 2) {
            payload = format!("[{{a:{payload}}}]");
            payload = format!("{{x:[{payload}]}}");
        }
        let component = format!(
            r#"{{text:"x",clickEvent:{{action:"custom",id:"minecraft:test",payload:{payload}}}}}"#
        );
        assert!(parse(&component).is_err(), "deep payload must be refused");
    }
}
