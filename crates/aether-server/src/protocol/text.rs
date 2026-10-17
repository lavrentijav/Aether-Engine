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
    Custom { id: String, payload: Option<Nbt> },
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
        /// Pre-1.20.5 `tag` (SNBT string).
        tag: Option<String>,
        /// 1.20.5+ data components.
        components: Option<Nbt>,
    },
    /// Show an entity.
    Entity {
        kind: Option<String>,
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
                *chars += fallback.as_deref().map(str::len).unwrap_or(0);
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
        if let Some(HoverEvent::Text(c)) = &self.style.hover_event {
            c.check_limits_at(depth + 1, nodes, chars)?;
        }
        if *chars > MAX_TEXT_CHARS {
            return Err("component text is too long".into());
        }
        Ok(())
    }

    /// The pre-1.20.3 JSON spelling, also the argument form clients show.
    pub fn to_json(&self) -> String {
        let mut fields: Vec<(String, String)> = Vec::new();
        self.content.push_json(&mut fields);
        self.style.push_json(&mut fields);
        if !self.extra.is_empty() {
            fields.push(("extra".into(), json_list(&self.extra)));
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

impl Content {
    fn push_json(&self, fields: &mut Vec<(String, String)>) {
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
                    fields.push(("with".into(), json_list(with)));
                }
            }
            Content::Selector {
                selector,
                separator,
            } => {
                fields.push(("selector".into(), quote(selector)));
                if let Some(s) = separator {
                    fields.push(("separator".into(), s.to_json()));
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
                    fields.push(("separator".into(), s.to_json()));
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
    fn push_json(&self, fields: &mut Vec<(String, String)>) {
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
            fields.push(("hoverEvent".into(), h.to_json()));
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
    fn to_json(&self) -> String {
        match self {
            HoverEvent::Text(c) => json_object(vec![
                ("action".into(), quote("show_text")),
                // Pre-1.21.5 spelling. 1.21.5 renamed it to `value`.
                ("contents".into(), c.to_json()),
            ]),
            HoverEvent::Item {
                id,
                count,
                tag,
                components,
            } => {
                let mut fields = vec![
                    ("action".into(), quote("show_item")),
                    ("id".into(), quote(id)),
                    ("count".into(), count.to_string()),
                ];
                if let Some(t) = tag {
                    fields.push(("tag".into(), quote(t)));
                } else if let Some(c) = components {
                    fields.push(("components".into(), nbt_to_json(c)));
                }
                json_object(fields)
            }
            HoverEvent::Entity { kind, id, name } => {
                let mut fields = vec![("action".into(), quote("show_entity"))];
                if let Some(k) = kind {
                    fields.push(("type".into(), quote(k)));
                }
                fields.push(("id".into(), quote(id)));
                if let Some(n) = name {
                    fields.push(("name".into(), n.to_json()));
                }
                json_object(fields)
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
                fields.push(("id".into(), Nbt::Str(id.clone())));
                fields.push(("count".into(), Nbt::Int(*count)));
                match form {
                    NbtForm::Legacy => {
                        if let Some(t) = tag {
                            fields.push(("tag".into(), Nbt::Str(t.clone())));
                        } else if let Some(c) = components {
                            fields.push(("components".into(), c.clone()));
                        }
                    }
                    NbtForm::Modern => {
                        if let Some(c) = components {
                            fields.push(("components".into(), c.clone()));
                        }
                    }
                }
            }
            HoverEvent::Entity { kind, id, name } => {
                fields.push(("action".into(), Nbt::Str("show_entity".into())));
                if let Some(k) = kind {
                    fields.push(("type".into(), Nbt::Str(k.clone())));
                }
                fields.push(("id".into(), Nbt::Str(id.clone())));
                if let Some(n) = name {
                    fields.push(("name".into(), n.to_nbt(form)));
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
                Content::Score { name, objective }
            } else if let Some(v) = field(fields, "keybind") {
                Content::Keybind(as_str(v, "keybind")?.to_string())
            } else if let Some(v) = field(fields, "nbt") {
                let path = as_str(v, "nbt")?.to_string();
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
        style.color = Some(as_str(v, "color")?.to_string());
    }
    if let Some(v) = field(fields, "font") {
        style.font = Some(as_str(v, "font")?.to_string());
    }
    style.bold = parse_optional_bool(fields, "bold")?;
    style.italic = parse_optional_bool(fields, "italic")?;
    style.underlined = parse_optional_bool(fields, "underlined")?;
    style.strikethrough = parse_optional_bool(fields, "strikethrough")?;
    style.obfuscated = parse_optional_bool(fields, "obfuscated")?;
    if let Some(v) = field(fields, "insertion") {
        style.insertion = Some(as_str(v, "insertion")?.to_string());
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
        "open_url" => ClickEvent::OpenUrl(value("url")?),
        "open_file" => ClickEvent::OpenFile(value("path")?),
        "run_command" => ClickEvent::RunCommand(value("command")?),
        "suggest_command" => ClickEvent::SuggestCommand(value("command")?),
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
        "custom" => {
            let id = get_str(fields, "id")?;
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
            let id = get_str(fields, "id")?;
            let count = match field(fields, "count") {
                Some(v) => as_i32(v, "count")?,
                None => 1,
            };
            let tag = match field(fields, "tag") {
                Some(Nbt::Str(s)) => Some(s.clone()),
                _ => None,
            };
            let components = field(fields, "components").cloned();
            HoverEvent::Item {
                id,
                count,
                tag,
                components,
            }
        }
        "show_entity" => {
            let id = get_str(fields, "id")?;
            let kind = match field(fields, "type") {
                Some(v) => Some(as_str(v, "type")?.to_string()),
                None => None,
            };
            let name = match field(fields, "name") {
                Some(v) => Some(Box::new(parse_component(v, depth + 1, nodes)?)),
                None => None,
            };
            HoverEvent::Entity { kind, id, name }
        }
        "custom" => {
            let id = get_str(fields, "id")?;
            let payload = field(fields, "payload").cloned();
            HoverEvent::Custom { id, payload }
        }
        other => return Err(format!("unknown hover event action '{other}'")),
    })
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

fn json_list(items: &[TextComponent]) -> String {
    let body = items
        .iter()
        .map(|c| c.to_json())
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
}
