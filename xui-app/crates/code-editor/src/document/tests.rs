use super::*;

#[test]
fn a_uniform_crlf_file_is_detected() {
    assert_eq!(LineEnding::detect("a\r\nb\r\n"), LineEnding::CrLf);
}

#[test]
fn a_uniform_lf_file_is_detected() {
    assert_eq!(LineEnding::detect("a\nb\n"), LineEnding::Lf);
}

#[test]
fn no_terminator_defaults_to_lf() {
    assert_eq!(LineEnding::detect("just one line"), LineEnding::Lf);
    assert_eq!(LineEnding::detect(""), LineEnding::Lf);
}

#[test]
fn mixed_endings_take_the_majority() {
    assert_eq!(LineEnding::detect("a\r\nb\r\nc\n"), LineEnding::CrLf);
    assert_eq!(LineEnding::detect("a\nb\nc\r\n"), LineEnding::Lf);
    assert_eq!(
        LineEnding::detect("a\r\nb\n"),
        LineEnding::Lf,
        "a tie is LF"
    );
}

#[test]
fn normalising_folds_crlf_and_lone_cr_to_lf() {
    assert_eq!(normalize("a\r\nb\rc\nd"), "a\nb\nc\nd");
}

#[test]
fn encoding_round_trips_each_ending() {
    let lf = Document::untitled();
    assert_eq!(lf.encode("a\nb"), b"a\nb");

    let crlf = Document {
        line_ending: LineEnding::CrLf,
        ..Document::untitled()
    };
    assert_eq!(crlf.encode("a\nb"), b"a\r\nb");
    assert_eq!(crlf.encode("a\r\nb"), b"a\r\nb", "an already-CRLF buffer");
}

#[test]
fn encoding_prefixes_a_bom() {
    let bom = Document {
        bom: true,
        ..Document::untitled()
    };
    assert_eq!(bom.encode("x"), b"\xEF\xBB\xBFx");
}
