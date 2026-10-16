//! The one internal frame both protocols decode into, and the routing rule that
//! decides who hears it.
//!
//! Opus is not re-encoded when forwarding: both Simple Voice Chat and Plasmo
//! Voice send Opus frames, and the plugin passes the payload through untouched.
//! The only conversion between protocols is the envelope (SVC's per-player
//! AES-GCM versus Plasmo's AES-CBC + source framing), which is a re-wrap, not a
//! decode/encode of the audio.
//!
//! [`route`] is pure: given the sender, the listeners and the config distances,
//! it returns who should receive the frame and why. Keeping it free of sockets
//! and locks is what makes cross-protocol proximity testable without a network.

/// Which server side a frame or a listener belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Svc,
    Plasmo,
}

/// One Opus audio frame, decoded from either protocol.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceFrame {
    pub protocol: Protocol,
    /// The speaker's UUID.
    pub sender: u128,
    /// The speaker's name, for logs and recordings.
    pub sender_name: String,
    /// The Opus payload, untouched.
    pub data: Vec<u8>,
    /// The proximity radius the speaker chose (SVC's whisper distance or the
    /// activation distance).
    pub distance: f64,
    /// Whether the speaker flagged the frame as a whisper.
    pub whispering: bool,
    /// The group the speaker is in, if any. A group frame reaches its members
    /// regardless of distance.
    pub group: Option<u128>,
}

/// A player that can hear, with the facts proximity needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Listener {
    pub uuid: u128,
    pub protocol: Protocol,
    /// The listener's world, so a cross-world listener is out of range.
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// The listener's group, if any.
    pub group: Option<u128>,
    /// Whether the listener has voice disabled (`update_state disabled`).
    pub disabled: bool,
}

/// Why a listener receives a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Within the speaker's proximity radius.
    Proximity,
    /// A member of the speaker's group.
    Group,
}

/// Decide who hears `frame`, given the speaker's world and position.
///
/// The rules mirror the two mods' own:
/// * a group member always hears the group, at any distance;
/// * otherwise a listener hears it when they are in the same world and within
///   `frame.distance` blocks;
/// * a listener with voice disabled hears nothing.
pub fn route(
    frame: &VoiceFrame,
    sender_world: &str,
    sender_x: f64,
    sender_y: f64,
    sender_z: f64,
    listeners: &[Listener],
) -> Vec<(u128, Route)> {
    let mut out = Vec::new();
    for l in listeners {
        if l.disabled {
            continue;
        }
        if frame.group.is_some() && frame.group == l.group {
            // Group audio crosses distance and worlds, exactly as the mods
            // forward it to the group's members.
            if l.uuid != frame.sender {
                out.push((l.uuid, Route::Group));
            }
            continue;
        }
        if l.world != sender_world {
            continue;
        }
        let dx = l.x - sender_x;
        let dy = l.y - sender_y;
        let dz = l.z - sender_z;
        let dist_sq = dx * dx + dy * dy + dz * dz;
        if dist_sq <= frame.distance * frame.distance && l.uuid != frame.sender {
            out.push((l.uuid, Route::Proximity));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(sender: u128, distance: f64, group: Option<u128>) -> VoiceFrame {
        VoiceFrame {
            protocol: Protocol::Svc,
            sender,
            sender_name: "sender".to_string(),
            data: vec![1, 2, 3],
            distance,
            whispering: false,
            group,
        }
    }

    fn listener(uuid: u128, x: f64, group: Option<u128>) -> Listener {
        Listener {
            uuid,
            protocol: Protocol::Plasmo,
            world: "minecraft:overworld".to_string(),
            x,
            y: 0.0,
            z: 0.0,
            group,
            disabled: false,
        }
    }

    #[test]
    fn proximity_uses_the_frame_radius_in_the_same_world() {
        let f = frame(1, 32.0, None);
        let listeners = vec![
            listener(2, 10.0, None), // in range
            listener(3, 40.0, None), // out of range
            listener(1, 0.0, None),  // the speaker themself
        ];
        let got = route(&f, "minecraft:overworld", 0.0, 0.0, 0.0, &listeners);
        assert_eq!(got, vec![(2, Route::Proximity)]);
    }

    #[test]
    fn a_group_crosses_distance_and_protocol() {
        let f = frame(1, 1.0, Some(99));
        let mut far = listener(2, 1000.0, Some(99));
        far.protocol = Protocol::Plasmo;
        let listeners = vec![far, listener(3, 50.0, Some(7))];
        let got = route(&f, "minecraft:overworld", 0.0, 0.0, 0.0, &listeners);
        assert_eq!(got, vec![(2, Route::Group)]);
    }

    #[test]
    fn disabled_and_cross_world_listeners_are_out() {
        let f = frame(1, 48.0, None);
        let mut disabled = listener(2, 1.0, None);
        disabled.disabled = true;
        let mut elsewhere = listener(3, 1.0, None);
        elsewhere.world = "minecraft:nether".to_string();
        let listeners = vec![disabled, elsewhere, listener(4, 2.0, None)];
        let got = route(&f, "minecraft:overworld", 0.0, 0.0, 0.0, &listeners);
        assert_eq!(got, vec![(4, Route::Proximity)]);
    }
}
