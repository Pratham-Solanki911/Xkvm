use kvm_common::display::{detect_edge_transition, scale_coordinates, DisplayInfo, ScreenEdge};
use kvm_common::{deserialize_envelope, serialize_envelope, Caps, Envelope, Event, MouseButton};

#[test]
fn test_all_envelope_variant_roundtrips() {
    let envelopes = vec![
        Envelope::Hello {
            name: "client-1".to_string(),
            version: "0.2.0".to_string(),
            caps: Caps::default(),
        },
        Envelope::HelloAck {
            file_transfer_port: 4001,
            server_name: "srv".to_string(),
        },
        Envelope::PairRequest {
            nonce: [1; 16],
            fingerprint: "abc123def456".to_string(),
            proof: [9; 32],
        },
        Envelope::PairAccept,
        Envelope::PairReject {
            reason: "Not allowed".to_string(),
        },
        Envelope::ToggleForward(true),
        Envelope::Input(Event::MouseDelta { dx: -100, dy: 200 }),
        Envelope::Input(Event::MouseButtonPress(MouseButton::Left)),
        Envelope::Input(Event::MouseButtonRelease(MouseButton::Right)),
        Envelope::Input(Event::Wheel {
            delta_x: 0,
            delta_y: 120,
        }),
        Envelope::Input(Event::KeyPress {
            code: 30,
            modifiers: 1,
        }),
        Envelope::Input(Event::KeyRelease {
            code: 30,
            modifiers: 0,
        }),
        Envelope::Input(Event::ReleaseAll),
        Envelope::ClipboardSet {
            text: "Emoji test: 🔒 🛡️ ⚡".to_string(),
        },
        Envelope::Ping(123456789),
        Envelope::Pong(123456789),
        Envelope::Goodbye,
    ];

    for env in envelopes {
        let ser = serialize_envelope(&env).expect("Serialization failed");
        assert!(ser.len() > 4);
        let de = deserialize_envelope(&ser[4..]).expect("Deserialization failed");
        let ser2 = serialize_envelope(&de).expect("Reserialization failed");
        assert_eq!(ser, ser2);
    }
}

#[test]
fn test_path_traversal_sanitization() {
    let malicious_paths = vec![
        "../../../../etc/passwd",
        "C:\\Windows\\System32\\cmd.exe",
        "..\\..\\sensitive_data.txt",
        "nested/dir/target.zip",
    ];

    for path in malicious_paths {
        let safe_name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("received_file");

        assert!(!safe_name.contains('/'));
        assert!(!safe_name.contains('\\'));
        assert!(!safe_name.contains(".."));
    }
}

#[test]
fn test_display_scaling_extreme_aspect_ratios() {
    let ultrawide = DisplayInfo {
        width: 3440,
        height: 1440,
        scale_factor: 1.0,
    };
    let portrait = DisplayInfo {
        width: 1080,
        height: 1920,
        scale_factor: 1.0,
    };

    let (sx, sy) = scale_coordinates(1720, 720, &ultrawide, &portrait);
    assert_eq!((sx, sy), (540, 960));
}

#[test]
fn test_edge_transition_corners() {
    let display = DisplayInfo {
        width: 1920,
        height: 1080,
        scale_factor: 1.0,
    };

    assert_eq!(
        detect_edge_transition(0, 0, &display, 5),
        Some(ScreenEdge::Left)
    );
    assert_eq!(
        detect_edge_transition(1919, 1079, &display, 5),
        Some(ScreenEdge::Right)
    );
}
