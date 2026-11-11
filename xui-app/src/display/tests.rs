use super::*;

#[test]
fn a_wheel_event_decodes_with_its_position_and_signed_delta() {
    for delta in [1, -1, 5, -120] {
        let body = wire::encode_pointer_wheel_args(&wire::PointerWheelArgs {
            x: -3,
            y: 44,
            delta,
        })
        .expect("encodes");
        let parcel = request(wire::METHOD_POINTERWHEEL, body, Vec::new());
        assert_eq!(
            decode_event(&parcel),
            Some(Event::PointerWheel {
                x: -3,
                y: 44,
                delta
            })
        );
    }
}

#[test]
fn a_truncated_wheel_body_is_not_an_event() {
    let parcel = request(wire::METHOD_POINTERWHEEL, vec![1, 2], Vec::new());
    assert_eq!(decode_event(&parcel), None);
}

/// A `Configure` event parcel carrying `body`.
fn configure(body: Vec<u8>) -> Parcel {
    request(wire::METHOD_CONFIGURE, body, Vec::new())
}

#[test]
fn configure_decodes_into_its_own_event() {
    let body = wire::encode_configure_args(&wire::ConfigureArgs {
        surface: 9,
        width: 950,
        height: 696,
        state: wire::WINDOW_STATE_MAXIMIZED,
    })
    .unwrap();
    assert_eq!(
        decode_event(&configure(body)),
        Some(Event::Configure {
            width: 950,
            height: 696,
            state: wire::WINDOW_STATE_MAXIMIZED,
        })
    );
}

#[test]
fn a_configure_with_an_oversized_size_is_clamped_not_wrapped() {
    let body = wire::encode_configure_args(&wire::ConfigureArgs {
        surface: 1,
        width: u32::MAX,
        height: 1,
        state: wire::WINDOW_STATE_NORMAL,
    })
    .unwrap();
    assert_eq!(
        decode_event(&configure(body)),
        Some(Event::Configure {
            width: i32::MAX,
            height: 1,
            state: wire::WINDOW_STATE_NORMAL,
        })
    );
    // An empty body decodes to a zero size, which `apply_configure` drops.
    assert_eq!(
        decode_event(&configure(Vec::new())),
        Some(Event::Configure {
            width: 0,
            height: 0,
            state: wire::WINDOW_STATE_NORMAL,
        })
    );
}
