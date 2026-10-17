use aether_plugin::worldedit::WorldEditHost;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pattern {
    Weighted {
        choices: Vec<(u32, String)>,
        total: u32,
        seed: u64,
    },
    Existing,
    Clipboard,
}

impl Pattern {
    pub fn pick(
        &self,
        host: &dyn WorldEditHost,
        caller: &str,
        x: i32,
        y: i32,
        z: i32,
    ) -> Result<String, String> {
        match self {
            Pattern::Weighted {
                choices,
                total,
                seed,
            } => {
                if choices.len() == 1 {
                    return Ok(choices[0].1.clone());
                }
                let roll = (mix(*seed, x, y, z) % *total as u64) as u32;
                let mut upto = 0;
                for (weight, name) in choices {
                    upto += *weight;
                    if roll < upto {
                        return Ok(name.clone());
                    }
                }
                Ok(choices[choices.len() - 1].1.clone())
            }
            Pattern::Existing => host
                .block_name(caller, x, y, z)
                .ok_or_else(|| format!("no block at {x} {y} {z}")),
            Pattern::Clipboard => Err("the clipboard is empty".to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    Any,
    Existing,
    Blocks(Vec<String>),
}

impl Filter {
    pub fn matches(&self, name: &str) -> bool {
        match self {
            Filter::Any => true,
            Filter::Existing => base(name) != "minecraft:air",
            Filter::Blocks(names) => names.iter().any(|n| base(n) == base(name)),
        }
    }
}

fn base(name: &str) -> &str {
    name.split_once('[').map_or(name, |(head, _)| head)
}

fn mix(seed: u64, x: i32, y: i32, z: i32) -> u64 {
    let mut v = seed
        ^ (x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (z as i64 as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    v ^= v >> 30;
    v = v.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    v ^= v >> 27;
    v = v.wrapping_mul(0x94D0_49BB_1331_11EB);
    v ^ (v >> 31)
}

fn weight_prefix(part: &str) -> Option<(u32, &str)> {
    for marker in ['%', ':'] {
        if let Some((head, tail)) = part.split_once(marker) {
            if !head.is_empty() && head.bytes().all(|b| b.is_ascii_digit()) {
                let weight = head.parse::<u32>().ok()?;
                return Some((weight, tail));
            }
        }
    }
    None
}

pub fn parse_pattern(
    text: &str,
    host: &dyn WorldEditHost,
    caller: &str,
    player: u64,
    pos1: Option<[i32; 3]>,
) -> Result<Pattern, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("expected a pattern".to_string());
    }
    if text.starts_with('#') {
        return match text {
            "#existing" => Ok(Pattern::Existing),
            "#clipboard" => Ok(Pattern::Clipboard),
            "#hand" => {
                let block = host
                    .held_block(caller, player)
                    .ok_or_else(|| "the held item is not a block".to_string())?;
                Ok(single(block))
            }
            "#pos1" => {
                let pos = pos1.ok_or_else(|| "position 1 is not set".to_string())?;
                let block = host
                    .block_name(caller, pos[0], pos[1], pos[2])
                    .ok_or_else(|| "position 1 is outside the world".to_string())?;
                Ok(single(block))
            }
            other => Err(format!("unknown pattern `{other}`")),
        };
    }
    let mut choices = Vec::new();
    let mut total = 0u32;
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err("a pattern entry is empty".to_string());
        }
        let (weight, name) = match weight_prefix(part) {
            Some((weight, name)) => (weight, name.trim()),
            None => (1, part),
        };
        if weight == 0 {
            return Err(format!("pattern `{part}` has a zero weight"));
        }
        if name.is_empty() {
            return Err(format!("pattern `{part}` has no block"));
        }
        let block = host
            .resolve_block(caller, name)
            .ok_or_else(|| format!("unknown block `{name}`"))?;
        total = total
            .checked_add(weight)
            .ok_or_else(|| "pattern weights are too large".to_string())?;
        choices.push((weight, block));
    }
    if choices.is_empty() {
        return Err("expected a pattern".to_string());
    }
    let seed = choices
        .iter()
        .fold(0xC0FF_EE00_1234_5678u64, |acc, (weight, name)| {
            let mut h = acc ^ (*weight as u64);
            for b in name.bytes() {
                h = h.wrapping_mul(1099511628211).wrapping_add(b as u64);
            }
            h
        });
    Ok(Pattern::Weighted {
        choices,
        total,
        seed,
    })
}

pub fn parse_filter(text: &str, host: &dyn WorldEditHost, caller: &str) -> Result<Filter, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("expected a filter".to_string());
    }
    if text == "#existing" {
        return Ok(Filter::Existing);
    }
    let mut names = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err("a filter entry is empty".to_string());
        }
        let (_, name) = match weight_prefix(part) {
            Some((weight, name)) => (weight, name.trim()),
            None => (1, part),
        };
        if name.is_empty() {
            return Err(format!("filter `{part}` has no block"));
        }
        if name.starts_with('#') {
            return Err(format!("unknown filter `{name}`"));
        }
        let block = host
            .resolve_block(caller, name)
            .ok_or_else(|| format!("unknown block `{name}`"))?;
        names.push(block);
    }
    Ok(Filter::Blocks(names))
}

fn single(block: String) -> Pattern {
    let seed = block.bytes().fold(0xC0FF_EE00_1234_5678u64, |h, b| {
        h.wrapping_mul(1099511628211).wrapping_add(b as u64)
    });
    Pattern::Weighted {
        choices: vec![(1, block)],
        total: 1,
        seed,
    }
}
