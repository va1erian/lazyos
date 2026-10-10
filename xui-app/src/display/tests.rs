use super::*;

#[test]
fn the_hint_parcel_carries_method_30_and_the_rect() {
    let parcel = hint_open_origin_parcel(7, (-4, 12, 64, 48)).expect("encodes");
    assert_eq!(parcel.header.method, 30);
    assert_eq!(parcel.header.interface_id, INTERFACE);
    let args = wire::decode_hint_open_origin_args(&parcel.body).expect("decodes");
    assert_eq!(
        (args.surface, args.x, args.y, args.w, args.h),
        (7, -4, 12, 64, 48)
    );
}

#[test]
fn the_size_request_parcel_carries_method_34_and_the_size() {
    let parcel = request_size_parcel(3, 200, 90).expect("encodes");
    assert_eq!(parcel.header.method, 34);
    let args = wire::decode_request_size_args(&parcel.body).expect("decodes");
    assert_eq!((args.surface, args.width, args.height), (3, 200, 90));
}

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
