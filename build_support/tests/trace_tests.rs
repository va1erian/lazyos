//! `LAZYOS_TRACE_MASK` and the old switches -> `trace.mask=`
//! (build_support/trace_cfg.rs).

use crate::os_image::trace_cfg::mask_text;

#[test]
fn nothing_set_writes_nothing() {
    assert_eq!(mask_text(None, &[]), Ok(None));
    assert_eq!(mask_text(Some("  "), &[]), Ok(None));
}

#[test]
fn the_mask_is_passed_through() {
    assert_eq!(
        mask_text(Some("init,pkgd,-shell"), &[]),
        Ok(Some("init,pkgd,-shell".to_string()))
    );
}

#[test]
fn an_alias_adds_its_channel_to_the_default_mask() {
    assert_eq!(
        mask_text(None, &["label"]),
        Ok(Some("default,label".to_string()))
    );
    assert_eq!(
        mask_text(Some("init"), &["label", "ui"]),
        Ok(Some("init,label,ui".to_string()))
    );
}

#[test]
fn a_misspelt_channel_fails_the_build() {
    let error = mask_text(Some("init,nosuch"), &[]).unwrap_err();
    assert!(error.contains("nosuch"), "{error}");
}

#[test]
fn every_alias_names_a_channel() {
    for (_, channel) in crate::os_image::trace_cfg::ALIASES {
        assert!(tracefmt::Channel::from_name(channel).is_some(), "{channel}");
    }
}

#[test]
fn the_line_reaches_the_kernel_parser() {
    let text = mask_text(Some("init,-shell"), &["label"]).unwrap().unwrap();
    let mask = tracefmt::Mask::parse(&text, |_| {});
    assert!(mask.allows(tracefmt::Channel::Init));
    assert!(mask.allows(tracefmt::Channel::Label));
    assert!(!mask.allows(tracefmt::Channel::Shell));
}
