//! The web admin's pages: plain, server-rendered HTML with a little inline
//! JavaScript for auto-refresh. No framework and no external CDN — every byte
//! is served from the plugin.

use aether_ops::{InventoryView, OpsService, PlayerProfile, RegionStat};

/// Escape text for HTML text and attribute contexts.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

const STYLE: &str = r#"
:root{--bg:#12151c;--card:#1b202b;--ink:#e6e9f0;--dim:#93a0b5;--accent:#5b9dff;--warn:#ffb057;--bad:#ff6b6b;--ok:#57d38c}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:14px/1.45 system-ui,Segoe UI,Roboto,sans-serif}
header{display:flex;gap:16px;align-items:center;padding:12px 20px;background:var(--card);border-bottom:1px solid #2a3140}
header b{color:var(--accent)}header a{color:var(--ink);text-decoration:none;opacity:.8}header a:hover{opacity:1}
main{padding:20px;max-width:1200px;margin:0 auto}
h1{font-size:20px;margin:0 0 16px}h2{font-size:16px;margin:24px 0 8px}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:12px}
.card{background:var(--card);border:1px solid #2a3140;border-radius:10px;padding:14px}
.card .k{color:var(--dim);font-size:12px;text-transform:uppercase;letter-spacing:.04em}
.card .v{font-size:24px;margin-top:4px}
table{width:100%;border-collapse:collapse;background:var(--card);border-radius:10px;overflow:hidden}
th,td{text-align:left;padding:8px 12px;border-bottom:1px solid #2a3140}
th{color:var(--dim);font-size:12px;text-transform:uppercase;letter-spacing:.04em}
tr:last-child td{border-bottom:none}a{color:var(--accent)}
.heat{display:grid;gap:2px;margin-top:12px}
.heat .c{width:22px;height:22px;border-radius:3px;position:relative}
.muted{color:var(--dim)}.bad{color:var(--bad)}.ok{color:var(--ok)}.warn{color:var(--warn)}
.inv{display:grid;grid-template-columns:repeat(9,1fr);gap:6px;margin-top:8px}
.slot{background:var(--card);border:1px solid #2a3140;border-radius:8px;min-height:52px;padding:6px;font-size:12px;overflow:hidden}
.slot .n{color:var(--ink);word-break:break-word}.slot .c{color:var(--dim)}
.slot.held{border-color:var(--accent)}.slot.armor{border-color:#4a5568}
.login{max-width:380px;margin:12vh auto}
input{width:100%;padding:10px;border-radius:8px;border:1px solid #2a3140;background:#0e1117;color:var(--ink)}
button{margin-top:10px;width:100%;padding:10px;border:0;border-radius:8px;background:var(--accent);color:#06203f;font-weight:600;cursor:pointer}
"#;

fn layout(title: &str, body: &str, script: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>{t} · Aether web admin</title><style>{css}</style></head><body>\
<header><b>Aether</b> <a href=\"/\">Dashboard</a> <a href=\"/regions\">Regions</a> \
<a href=\"/players\">Players</a> <a href=\"/map\">Map</a><span class=\"muted\" style=\"margin-left:auto\">read-only</span></header>\
<main>{body}</main>{script}</body></html>",
        t = esc(title),
        css = STYLE,
        body = body,
        script = script,
    )
}

/// The login form.
pub fn login(error: &str) -> String {
    let message = if error.is_empty() {
        String::new()
    } else {
        format!("<p class=\"bad\">{}</p>", esc(error))
    };
    let body = format!(
        "<div class=\"login\"><h1>Web admin</h1>\
<p class=\"muted\">Enter the operator token. It is read from <code>[plugins.webadmin] token</code>, \
or minted in game with <code>/webadmin token</code>.</p>{message}\
<form method=\"post\" action=\"/login\"><input type=\"password\" name=\"token\" \
placeholder=\"token\" autofocus autocomplete=\"current-password\"><button type=\"submit\">Sign in</button></form></div>"
    );
    layout("Login", &body, "")
}

fn num_card(key: &str, value: &str) -> String {
    format!(
        "<div class=\"card\"><div class=\"k\">{}</div><div class=\"v\">{}</div></div>",
        esc(key),
        value
    )
}

/// The dashboard.
pub fn dashboard(ops: &dyn OpsService) -> String {
    let d = ops.dashboard();
    let body = format!(
        "<h1>Dashboard</h1><div class=\"cards\">\
{d_tps}{d_mspt}{d_msptmax}{d_players}{d_mem}{d_uptime}{d_ticks}{d_chunks}</div>\
<h2>Heaviest regions</h2><div id=\"regions\"></div>\
<p class=\"muted\">Sampled once a second. Entity and container counts are instantaneous; \
block updates are scheduled block/fluid ticks pending in the region; chunk sends are columns \
sent since the last sample. Redstone is zero until the redstone engine is wired.</p>",
        d_tps = num_card("TPS", &format!("<span id=\"tps\">{:.1}</span>", d.tps)),
        d_mspt = num_card(
            "MSPT",
            &format!("<span id=\"mspt\">{:.2} ms</span>", d.mspt)
        ),
        d_msptmax = num_card(
            "Slowest tick",
            &format!("<span id=\"msptmax\">{:.2} ms</span>", d.mspt_max)
        ),
        d_players = num_card(
            "Players",
            &format!(
                "<span id=\"players\">{} / {}</span>",
                d.players_online, d.players_max
            )
        ),
        d_mem = num_card(
            "Memory",
            &format!(
                "<span id=\"mem\">{} / {} MiB</span>",
                d.memory_used_mib, d.memory_total_mib
            )
        ),
        d_uptime = num_card(
            "Uptime",
            &format!("<span id=\"uptime\">{}</span>", duration(d.uptime_secs))
        ),
        d_ticks = num_card("Ticks", &format!("<span id=\"ticks\">{}</span>", d.ticks)),
        d_chunks = num_card(
            "Columns sent",
            &format!("<span id=\"chunks\">{}</span>", d.chunk_sends)
        ),
    );
    let script = "<script>\
async function tick(){try{let r=await fetch('/api/dashboard');let d=await r.json();\
document.getElementById('tps').textContent=d.tps.toFixed(1);\
document.getElementById('mspt').textContent=d.mspt.toFixed(2)+' ms';\
document.getElementById('msptmax').textContent=d.mspt_max.toFixed(2)+' ms';\
document.getElementById('players').textContent=d.players_online+' / '+d.players_max;\
document.getElementById('mem').textContent=d.memory_used_mib+' / '+d.memory_total_mib+' MiB';\
document.getElementById('ticks').textContent=d.ticks;\
document.getElementById('chunks').textContent=d.chunk_sends;\
let rr=await fetch('/api/regions');let rs=await rr.json();\
document.getElementById('regions').innerHTML=regionTable(rs);}catch(e){}}\
function regionTable(rs){if(!rs.length)return '<p class=\"muted\">no activity sampled yet</p>';\
let h='<table><tr><th>region</th><th>cost</th><th>entities</th><th>block&nbsp;ents</th><th>block&nbsp;updates</th><th>chunk&nbsp;sends</th></tr>';\
for(const r of rs.slice(0,25)){h+='<tr><td>'+r.rx+', '+r.rz+'</td><td>'+r.cost.toFixed(1)+'</td><td>'+r.entities+'</td><td>'+r.block_entities+'</td><td>'+r.block_updates+'</td><td>'+r.chunk_sends+'</td></tr>';}\
return h+'</table>';}\
tick();setInterval(tick,2000);</script>";
    layout("Dashboard", &body, script)
}

/// The regions page: ranked table plus a heat grid.
pub fn regions(ops: &dyn OpsService) -> String {
    let rs = ops.regions(200);
    let body = if rs.is_empty() {
        "<h1>Heaviest regions</h1><p class=\"muted\">No activity sampled yet.</p>".to_string()
    } else {
        format!(
            "<h1>Heaviest regions</h1>{}{}",
            ranked_table(&rs),
            heat_grid(&rs)
        )
    };
    layout("Regions", &body, "")
}

fn ranked_table(rs: &[RegionStat]) -> String {
    let mut h = "<table><tr><th>region (rx, rz)</th><th>cost</th><th>entities</th>\
<th>block entities</th><th>block updates</th><th>redstone</th><th>chunk sends</th><th>samples</th></tr>"
        .to_string();
    for r in rs.iter().take(100) {
        h.push_str(&format!(
            "<tr><td>{}, {}</td><td>{:.1}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            r.rx, r.rz, r.cost, r.entities, r.block_entities, r.block_updates, r.redstone_updates, r.chunk_sends, r.samples
        ));
    }
    h.push_str("</table>");
    h
}

/// A coloured grid of the sampled regions, one cell per region, brightness by
/// cost. Cheap "heat overlay"; the real top-down map is WEB2.
fn heat_grid(rs: &[RegionStat]) -> String {
    let (mut minx, mut maxx, mut minz, mut maxz) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    let max_cost = rs.iter().map(|r| r.cost).fold(0.0f64, f64::max).max(1.0);
    for r in rs {
        minx = minx.min(r.rx);
        maxx = maxx.max(r.rx);
        minz = minz.min(r.rz);
        maxz = maxz.max(r.rz);
    }
    let w = maxx - minx + 1;
    let h = maxz - minz + 1;
    if w <= 0 || h <= 0 || (w as i64) * (h as i64) > 10_000 {
        return String::new();
    }
    let at: std::collections::HashMap<(i32, i32), &RegionStat> =
        rs.iter().map(|r| ((r.rx, r.rz), r)).collect();
    let mut cells = String::new();
    for rz in minz..=maxz {
        for rx in minx..=maxx {
            match at.get(&(rx, rz)) {
                Some(r) => {
                    let t = (r.cost / max_cost).clamp(0.0, 1.0);
                    let colour = heat_colour(t);
                    cells.push_str(&format!(
                        "<div class=\"c\" style=\"background:{colour}\" title=\"region {rx}, {rz}: cost {:.1}\"></div>",
                        r.cost
                    ));
                }
                None => cells.push_str("<div class=\"c\" style=\"background:#0e1117\"></div>"),
            }
        }
    }
    format!(
        "<h2>Heat overlay</h2><div class=\"heat\" style=\"grid-template-columns:repeat({w},22px)\">{cells}</div>\
<p class=\"muted\">One cell per 32×32-chunk region; brighter is hotter. Orientation: rx right, rz down.</p>"
    )
}

fn heat_colour(t: f64) -> String {
    // Cold blue -> hot red, through the accent.
    let r = (40.0 + 215.0 * t) as u8;
    let g = (90.0 + 80.0 * (1.0 - t)) as u8;
    let b = (255.0 * (1.0 - t) + 60.0 * t) as u8;
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// The players page.
pub fn players(ops: &dyn OpsService) -> String {
    let list = ops.players();
    let mut h = format!(
        "<h1>Players <span class=\"muted\">({})</span></h1>",
        list.len()
    );
    h.push_str("<table><tr><th>name</th><th>state</th><th>address</th><th>first seen</th><th>last seen</th><th>playtime</th><th></th></tr>");
    for p in &list {
        let state = if p.online {
            "<span class=\"ok\">online</span>"
        } else {
            "<span class=\"muted\">offline</span>"
        };
        h.push_str(&format!(
            "<tr><td><a href=\"/profile?id={uuid}\">{name}</a></td><td>{state}</td>\
<td class=\"muted\">{ip}</td><td class=\"muted\">{first}</td><td class=\"muted\">{last}</td>\
<td>{play}</td><td><a href=\"/inventory?id={uuid}\">inventory</a></td></tr>",
            uuid = esc(&p.uuid),
            name = esc(&p.name),
            state = state,
            ip = esc(p.ip.as_deref().unwrap_or("—")),
            first = iso8601(p.first_seen),
            last = iso8601(p.last_seen),
            play = duration(p.playtime_secs),
        ));
    }
    h.push_str("</table>");
    layout("Players", &h, "")
}

/// A single player's profile.
pub fn profile(p: &PlayerProfile) -> String {
    let state = if p.entry.online {
        "<span class=\"ok\">online</span>"
    } else {
        "<span class=\"muted\">offline</span>"
    };
    let cards = format!(
        "{}{}{}{}",
        num_card("First seen", &iso8601(p.entry.first_seen)),
        num_card("Last seen", &iso8601(p.entry.last_seen)),
        num_card("Playtime", &duration(p.entry.playtime_secs)),
        num_card("Session", &duration(p.session_secs)),
    );
    let body = format!(
        "<h1>{name}</h1><p class=\"muted\">{state} · <code>{uuid}</code> · \
<a href=\"/inventory?id={uuid}\">inventory</a></p>\
<div class=\"cards\">{cards}</div>\
<h2>Addresses seen</h2><p class=\"muted\">{ips}</p>",
        name = esc(&p.entry.name),
        uuid = esc(&p.entry.uuid),
        state = state,
        cards = cards,
        ips = if p.ips.is_empty() {
            "none recorded".to_string()
        } else {
            esc(&p.ips.join(", "))
        },
    );
    let mut body = body;
    if let Some(pos) = p.position {
        let health = p
            .health
            .map(|h| format!("{h:.1}"))
            .unwrap_or_else(|| "—".into());
        let food = p.food.map(|f| f.to_string()).unwrap_or_else(|| "—".into());
        let mode = p.game_mode.clone().unwrap_or_else(|| "—".into());
        body.push_str(&format!(
            "<h2>Live</h2><div class=\"cards\">{}{}{}{}</div>",
            num_card(
                "Position",
                &format!("{:.1}, {:.1}, {:.1}", pos.0, pos.1, pos.2)
            ),
            num_card("Health", &health),
            num_card("Food", &food),
            num_card("Mode", &esc(&mode)),
        ));
    }
    layout("Player", &body, "")
}

/// A player's inventory.
pub fn inventory(v: &InventoryView) -> String {
    let state = if v.online { "live" } else { "saved" };
    let mut body = format!(
        "<h1>{name} · inventory <span class=\"muted\">({state})</span></h1>\
<p class=\"muted\"><a href=\"/profile?id={uuid}\">profile</a> · read-only</p>",
        name = esc(&v.name),
        uuid = esc(&v.uuid),
        state = state,
    );
    if !v.armor.is_empty() {
        body.push_str("<h2>Armour</h2><div class=\"inv\">");
        for slot in &v.armor {
            body.push_str(&slot_html(slot, true, false));
        }
        body.push_str("</div>");
    }
    body.push_str(&format!(
        "<h2>Inventory <span class=\"muted\">(held slot {})</span></h2><div class=\"inv\">",
        v.held_slot
    ));
    if v.slots.is_empty() {
        body.push_str("</div><p class=\"muted\">empty</p>");
    } else {
        for slot in &v.slots {
            let held = slot.slot == 36 + v.held_slot as usize;
            body.push_str(&slot_html(slot, false, held));
        }
        body.push_str("</div>");
    }
    layout("Inventory", &body, "")
}

fn slot_html(slot: &aether_ops::ItemView, armor: bool, held: bool) -> String {
    let class = match (armor, held) {
        (true, _) => "slot armor",
        (_, true) => "slot held",
        _ => "slot",
    };
    format!(
        "<div class=\"{class}\" title=\"slot {slot_n}\"><div class=\"n\">{n}</div><div class=\"c\">×{c}{dmg}</div></div>",
        class = class,
        slot_n = slot.slot,
        n = esc(&slot.item),
        c = slot.count,
        dmg = if slot.damage > 0 {
            format!(" · wear {}", slot.damage)
        } else {
            String::new()
        },
    )
}

/// Human-readable duration: `3d 4h 5m`, `12s`.
fn duration(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86400,
        secs % 86400 / 3600,
        secs % 3600 / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// UTC timestamp for a Unix second, `—` for zero.
fn iso8601(secs: i64) -> String {
    if secs <= 0 {
        return "—".to_string();
    }
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_ops::{Dashboard, InventoryView, ItemView, PlayerEntry, PlayerProfile, RegionStat};

    struct Mock;
    impl OpsService for Mock {
        fn dashboard(&self) -> Dashboard {
            Dashboard {
                tps: 19.9,
                mspt: 50.0,
                mspt_max: 63.0,
                players_online: 2,
                players_max: 20,
                ticks: 1234,
                uptime_secs: 90_061,
                memory_used_mib: 100,
                memory_total_mib: 8000,
                chunk_sends: 42,
            }
        }
        fn regions(&self, _limit: usize) -> Vec<RegionStat> {
            vec![RegionStat {
                rx: 0,
                rz: 0,
                entities: 3,
                block_entities: 1,
                block_updates: 0,
                redstone_updates: 0,
                chunk_sends: 8,
                cost: 7.0,
                samples: 2,
            }]
        }
        fn players(&self) -> Vec<PlayerEntry> {
            vec![PlayerEntry {
                uuid: "a".repeat(32),
                name: "Steve".into(),
                online: true,
                ip: Some("127.0.0.1".into()),
                first_seen: 1_700_000_000,
                last_seen: 1_700_000_500,
                playtime_secs: 500,
            }]
        }
        fn profile(&self, id: &str) -> Option<PlayerProfile> {
            if id != "Steve" && id.len() != 32 {
                return None;
            }
            Some(PlayerProfile {
                entry: self.players().into_iter().next().unwrap(),
                ips: vec!["127.0.0.1".into()],
                session_secs: 60,
                position: Some((1.0, 64.0, 2.0)),
                health: Some(20.0),
                food: Some(20),
                game_mode: Some("survival".into()),
            })
        }
        fn inventory(&self, _id: &str) -> Option<InventoryView> {
            Some(InventoryView {
                uuid: "a".repeat(32),
                name: "Steve".into(),
                online: true,
                held_slot: 0,
                slots: vec![ItemView {
                    slot: 36,
                    item: "minecraft:diamond".into(),
                    count: 5,
                    damage: 0,
                    potion: None,
                }],
                armor: vec![ItemView {
                    slot: 5,
                    item: "minecraft:iron_helmet".into(),
                    count: 1,
                    damage: 12,
                    potion: None,
                }],
                ender: Vec::new(),
            })
        }
    }

    #[test]
    fn escapes_html_metacharacters() {
        assert_eq!(esc("<b>&\"'"), "&lt;b&gt;&amp;&quot;&#39;");
    }

    #[test]
    fn dashboard_renders_numbers_and_script() {
        let html = dashboard(&Mock);
        assert!(html.contains("Dashboard"));
        assert!(html.contains("19.9"));
        assert!(html.contains("/api/dashboard"));
        assert!(html.contains("setInterval"));
    }

    #[test]
    fn regions_renders_table_and_heat() {
        let html = regions(&Mock);
        assert!(html.contains("Heaviest regions"));
        assert!(html.contains("Heat overlay"));
        assert!(html.contains("#"), "a heat colour is emitted");
    }

    #[test]
    fn players_links_to_profile_and_inventory() {
        let html = players(&Mock);
        assert!(html.contains("/profile?id="));
        assert!(html.contains("/inventory?id="));
        assert!(html.contains("Steve"));
    }

    #[test]
    fn profile_and_inventory_render() {
        let p = profile(&Mock.profile("Steve").unwrap());
        assert!(p.contains("First seen"));
        assert!(p.contains("Position"));
        let inv = inventory(&Mock.inventory("x").unwrap());
        assert!(inv.contains("minecraft:diamond"));
        assert!(inv.contains("minecraft:iron_helmet"));
        assert!(inv.contains("wear 12"));
    }

    #[test]
    fn durations_and_dates_are_human_readable() {
        assert_eq!(duration(90061), "1d 1h 1m");
        assert_eq!(duration(45), "45s");
        assert_eq!(iso8601(0), "—");
        assert_eq!(iso8601(1_700_000_000), "2023-11-14 22:13 UTC");
    }
}
