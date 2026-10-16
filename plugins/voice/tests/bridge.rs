//! An end-to-end cross-protocol bridge test.
//!
//! It drives the plugin's pure layers exactly as the UDP threads do: a client
//! of one protocol encodes a packet, the server decodes it, turns it into the
//! one internal frame, routes it, and re-wraps it for a listener of the *other*
//! protocol — asserting the Opus payload arrives byte-for-byte unchanged. This
//! is what "players on Simple Voice Chat and Plasmo Voice hear each other"
//! reduces to, without a socket.

use aether_voice_plugin::frame::{self, Listener, Protocol, VoiceFrame};
use aether_voice_plugin::{plasmo, svc};

const SPEAKER: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
const LISTENER: u128 = 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000;

fn listener(protocol: Protocol) -> Listener {
    Listener {
        uuid: LISTENER,
        protocol,
        world: "minecraft:overworld".to_string(),
        x: 5.0,
        y: 64.0,
        z: 0.0,
        group: None,
        disabled: false,
    }
}

#[test]
fn svc_to_plasmo_keeps_the_opus_payload() {
    let opus = vec![0xab; 80];
    let svc_secret = svc::Secret([7u8; 16]);

    // 1. The SVC client sends a mic packet on the client envelope.
    let datagram = svc::Packet::Mic {
        data: opus.clone(),
        sequence: 1,
        whispering: false,
    }
    .encode_client_udp(SPEAKER, &svc_secret);

    // 2. The server decodes it (peek then decode, as the thread does).
    assert_eq!(svc::peek_player(&datagram), Some(SPEAKER));
    let decoded = svc::Packet::decode_client_udp(&datagram, &svc_secret).unwrap();
    let data = match decoded {
        svc::Packet::Mic { data, .. } => data,
        other => panic!("expected a mic packet, got {other:?}"),
    };

    // 3. One internal frame, then route it to the Plasmo listener.
    let vf = VoiceFrame {
        protocol: Protocol::Svc,
        sender: SPEAKER,
        sender_name: "Steve".to_string(),
        data: data.clone(),
        distance: 48.0,
        whispering: false,
        group: None,
    };
    let targets = frame::route(
        &vf,
        "minecraft:overworld",
        0.0,
        64.0,
        0.0,
        &[listener(Protocol::Plasmo)],
    );
    assert_eq!(targets, vec![(LISTENER, frame::Route::Proximity)]);

    // 4. Re-wrap as Plasmo source audio for that listener.
    let pv_secret = 0xdead_beef_dead_beef_dead_beef_dead_beefu128;
    let out = plasmo::Udp::SourceAudio(plasmo::SourceAudio {
        sequence: 1,
        data: data.clone(),
        source_id: 42,
        source_state: 0,
        distance: 48,
    })
    .encode(pv_secret);

    // 5. The Plasmo client decodes it and sees the same Opus bytes.
    let (secret, udp) = plasmo::Udp::decode(&out).unwrap();
    assert_eq!(secret, pv_secret);
    match udp {
        plasmo::Udp::SourceAudio(a) => assert_eq!(a.data, opus),
        other => panic!("expected source audio, got {other:?}"),
    }
}

#[test]
fn plasmo_to_svc_keeps_the_opus_payload() {
    let opus = vec![0xcd; 120];
    let pv_secret = 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10u128;

    // 1. The Plasmo client sends player audio.
    let datagram = plasmo::Udp::PlayerAudio(plasmo::PlayerAudio {
        sequence: 9,
        data: opus.clone(),
        activation: plasmo::proximity_activation_id(),
        distance: 32,
        stereo: false,
    })
    .encode(pv_secret);

    // 2. The server decodes it and finds the player by secret.
    let (secret, udp) = plasmo::Udp::decode(&datagram).unwrap();
    assert_eq!(secret, pv_secret);
    let data = match udp {
        plasmo::Udp::PlayerAudio(a) => a.data,
        other => panic!("expected player audio, got {other:?}"),
    };

    // 3. Route to an SVC listener.
    let vf = VoiceFrame {
        protocol: Protocol::Plasmo,
        sender: SPEAKER,
        sender_name: "Alex".to_string(),
        data: data.clone(),
        distance: 32.0,
        whispering: false,
        group: None,
    };
    let targets = frame::route(
        &vf,
        "minecraft:overworld",
        0.0,
        64.0,
        0.0,
        &[listener(Protocol::Svc)],
    );
    assert_eq!(targets.len(), 1);

    // 4. Re-wrap as an SVC player-sound packet for the listener's own secret.
    let svc_secret = svc::Secret([3u8; 16]);
    let out = svc::Packet::PlayerSound {
        channel_id: SPEAKER,
        sender: SPEAKER,
        data: data.clone(),
        sequence: 9,
        whispering: false,
        distance: 32.0,
        category: None,
    }
    .encode_server_udp(&svc_secret);

    // 5. The SVC client decodes it.
    let decoded = svc::Packet::decode_server_udp(&out, &svc_secret).unwrap();
    match decoded {
        svc::Packet::PlayerSound { data: got, .. } => assert_eq!(got, opus),
        other => panic!("expected player sound, got {other:?}"),
    }
}

#[test]
fn the_svc_handshake_secret_reaches_the_client() {
    // Client asks; server answers with the secret on `voicechat:secret`.
    let request = svc::ChannelPacket::RequestSecret {
        compatibility_version: 18,
    };
    let decoded =
        svc::ChannelPacket::decode(svc::channel::REQUEST_SECRET, &request.encode()).unwrap();
    assert_eq!(decoded, request);

    let secret = svc::Secret([0x42; 16]);
    let reply = svc::ChannelPacket::Secret(Box::new(svc::SecretData {
        secret,
        server_port: 24_454,
        player: SPEAKER,
        codec: svc::Codec::Voip as u8,
        mtu: 1275,
        distance: 48.0,
        keep_alive: 1000,
        groups_enabled: true,
        voice_host: String::new(),
        allow_recording: true,
    }));
    assert_eq!(reply.channel(), Some(svc::channel::SECRET));
    // The secret is the first 16 bytes of the payload.
    assert_eq!(&reply.encode()[..16], &[0x42; 16]);
}

#[test]
fn a_plasmo_player_info_produces_a_connection_secret() {
    let info = plasmo::Tcp::PlayerInfo {
        voice_disabled: false,
        microphone_muted: false,
        minecraft_version: "1.21.11".to_string(),
        version: "2.1.7".to_string(),
        public_key: vec![0x30, 0x82],
    };
    let decoded = plasmo::Tcp::decode(&info.encode()).unwrap();
    assert_eq!(decoded, info);

    // The server's answer carries a random secret, a host and a port.
    let connection = plasmo::Tcp::Connection {
        secret: 0xabc,
        ip: "127.0.0.1".to_string(),
        port: 60_606,
    };
    assert_eq!(connection.encode()[0], plasmo::tcp::CONNECTION);
}
