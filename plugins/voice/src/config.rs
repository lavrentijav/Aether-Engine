//! Operator configuration for the voice plugin.
//!
//! Read from `[plugins.voice]` in the server config (a flat string map, like
//! every native plugin's). Every value has a default so a fresh install works:
//! the Simple Voice Chat and Plasmo Voice servers start on their conventional
//! ports, proximity is 48 blocks (SVC's own default), and recording is **off**.

use std::collections::BTreeMap;

/// The plugin's configuration.
#[derive(Debug, Clone)]
pub struct VoiceConfig {
    /// Whether the Simple Voice Chat server side is started.
    pub svc_enabled: bool,
    /// The SVC UDP port (the mod's default).
    pub svc_port: u16,
    /// Whether the Plasmo Voice server side is started.
    pub pv_enabled: bool,
    /// The Plasmo Voice UDP port.
    pub pv_port: u16,
    /// How far normal speech carries, in blocks.
    pub distance: f64,
    /// How far a whisper carries, in blocks.
    pub whisper: f64,
    /// Whether groups are offered to SVC clients.
    pub groups_enabled: bool,
    /// The permission node an operator needs for the `/voice` admin commands.
    pub admin_permission: String,
    /// Where recordings are written (Ogg/Opus, one file per speaker/session).
    pub record_dir: String,
    /// Show players a notice when they are being recorded. On by default: the
    /// legal expectation in many jurisdictions is that a recorded party knows.
    pub notify_recorded: bool,
    /// Delete recordings older than this many days (`0` keeps them forever).
    pub retention_days: u32,
    /// Soft cap on the recording directory in mebibytes (`0` is no cap).
    pub max_disk_mb: u64,
    /// The AES-128 key Plasmo Voice protects audio with, if encryption is on.
    pub pv_aes_key: Option<[u8; 16]>,
}

impl Default for VoiceConfig {
    fn default() -> VoiceConfig {
        VoiceConfig {
            svc_enabled: true,
            svc_port: 24_454,
            pv_enabled: true,
            pv_port: 60_606,
            distance: 48.0,
            whisper: 24.0,
            groups_enabled: true,
            admin_permission: "aether.voice.admin".to_string(),
            record_dir: "voice-recordings".to_string(),
            notify_recorded: true,
            retention_days: 0,
            max_disk_mb: 0,
            pv_aes_key: None,
        }
    }
}

impl VoiceConfig {
    /// Read the operator's `[plugins.voice]` overrides.
    pub fn from_map(map: &BTreeMap<String, String>) -> VoiceConfig {
        let mut c = VoiceConfig::default();
        let b = |k: &str| map.get(k).map(|v| v.eq_ignore_ascii_case("true"));
        let u = |k: &str| map.get(k).and_then(|v| v.parse::<u64>().ok());
        let f = |k: &str| map.get(k).and_then(|v| v.parse::<f64>().ok());
        if let Some(v) = b("svc_enabled") {
            c.svc_enabled = v;
        }
        if let Some(v) = u("svc_port") {
            c.svc_port = v as u16;
        }
        if let Some(v) = b("pv_enabled") {
            c.pv_enabled = v;
        }
        if let Some(v) = u("pv_port") {
            c.pv_port = v as u16;
        }
        if let Some(v) = f("distance") {
            c.distance = v;
        }
        if let Some(v) = f("whisper") {
            c.whisper = v;
        }
        if let Some(v) = b("groups_enabled") {
            c.groups_enabled = v;
        }
        if let Some(v) = map.get("admin_permission") {
            c.admin_permission = v.clone();
        }
        if let Some(v) = map.get("record_dir") {
            c.record_dir = v.clone();
        }
        if let Some(v) = b("notify_recorded") {
            c.notify_recorded = v;
        }
        if let Some(v) = u("retention_days") {
            c.retention_days = v as u32;
        }
        if let Some(v) = u("max_disk_mb") {
            c.max_disk_mb = v;
        }
        if let Some(v) = map.get("pv_aes_key").and_then(|s| parse_hex16(s)) {
            c.pv_aes_key = Some(v);
        }
        c
    }
}

/// Parse a 32-character hex string into 16 bytes.
fn parse_hex16(s: &str) -> Option<[u8; 16]> {
    let s = s.trim();
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_mods() {
        let c = VoiceConfig::default();
        assert_eq!(c.svc_port, 24_454);
        assert_eq!(c.pv_port, 60_606);
        assert_eq!(c.distance, 48.0);
        assert!(!c.record_dir.is_empty());
    }

    #[test]
    fn operator_overrides_apply() {
        let mut m = BTreeMap::new();
        m.insert("svc_port".to_string(), "25000".to_string());
        m.insert("pv_enabled".to_string(), "false".to_string());
        m.insert("distance".to_string(), "64".to_string());
        m.insert(
            "pv_aes_key".to_string(),
            "000102030405060708090a0b0c0d0e0f".to_string(),
        );
        let c = VoiceConfig::from_map(&m);
        assert_eq!(c.svc_port, 25000);
        assert!(!c.pv_enabled);
        assert_eq!(c.distance, 64.0);
        assert_eq!(
            c.pv_aes_key,
            Some([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])
        );
    }
}
