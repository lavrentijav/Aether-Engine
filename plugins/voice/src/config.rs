//! Operator configuration for the voice plugin.
//!
//! Read from `[plugins.voice]` in the server config (a flat string map, like
//! every native plugin's). Every value has a default so a fresh install works.
//! The two UDP listeners are **opt-in** — `svc_enabled`/`pv_enabled` default to
//! `false`, so installing the plugin opens no port until the operator asks —
//! proximity is 48 blocks (SVC's own default), and recording is off.

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
}

impl Default for VoiceConfig {
    fn default() -> VoiceConfig {
        VoiceConfig {
            // The UDP listeners are opt-in: a fresh install opens no ports
            // until the operator enables them.
            svc_enabled: false,
            svc_port: 24_454,
            pv_enabled: false,
            pv_port: 60_606,
            distance: 48.0,
            whisper: 24.0,
            groups_enabled: true,
            admin_permission: "aether.voice.admin".to_string(),
            record_dir: "voice-recordings".to_string(),
            notify_recorded: true,
            retention_days: 0,
            max_disk_mb: 0,
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
        // A port must be a real `u16`; a value outside 1..=65535 is ignored, so
        // a mistyped value cannot silently wrap (`70000 as u16 == 4464`).
        let port = |k: &str| {
            map.get(k)
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|v| (1..=65_535).contains(v))
                .map(|v| v as u16)
        };
        if let Some(v) = b("svc_enabled") {
            c.svc_enabled = v;
        }
        if let Some(v) = port("svc_port") {
            c.svc_port = v;
        }
        if let Some(v) = b("pv_enabled") {
            c.pv_enabled = v;
        }
        if let Some(v) = port("pv_port") {
            c.pv_port = v;
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
        c
    }
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
        // The listeners are opt-in.
        assert!(!c.svc_enabled && !c.pv_enabled);
    }

    #[test]
    fn listeners_can_be_enabled() {
        let mut m = BTreeMap::new();
        m.insert("svc_enabled".to_string(), "true".to_string());
        m.insert("pv_enabled".to_string(), "true".to_string());
        let c = VoiceConfig::from_map(&m);
        assert!(c.svc_enabled && c.pv_enabled);
    }

    #[test]
    fn an_out_of_range_port_is_ignored() {
        let mut m = BTreeMap::new();
        // 70000 would wrap to 4464 with a bare `as u16`; it must be refused.
        m.insert("svc_port".to_string(), "70000".to_string());
        m.insert("pv_port".to_string(), "0".to_string());
        let c = VoiceConfig::from_map(&m);
        assert_eq!(c.svc_port, 24_454, "out-of-range port must be ignored");
        assert_eq!(c.pv_port, 60_606, "0 is not a valid port");
        // A real port still applies.
        m.insert("svc_port".to_string(), "24455".to_string());
        let c = VoiceConfig::from_map(&m);
        assert_eq!(c.svc_port, 24455);
    }

    #[test]
    fn operator_overrides_apply() {
        let mut m = BTreeMap::new();
        m.insert("svc_port".to_string(), "25000".to_string());
        m.insert("pv_enabled".to_string(), "false".to_string());
        m.insert("distance".to_string(), "64".to_string());
        let c = VoiceConfig::from_map(&m);
        assert_eq!(c.svc_port, 25000);
        assert!(!c.pv_enabled);
        assert_eq!(c.distance, 64.0);
    }
}
