use std::path::Path;

#[test]
fn shipped_catalogs_translate_every_key() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../locales");
    let catalogs =
        cap_i18n_build::read_catalogs(&dir).unwrap_or_else(|errors| panic!("{errors:?}"));
    let codes: Vec<&str> = catalogs.iter().map(|(code, _)| code.as_str()).collect();
    let mut expected: Vec<&str> = cap_i18n_build::LOCALES
        .iter()
        .map(|spec| spec.code)
        .collect();
    expected.sort();
    assert_eq!(codes, expected);
    let borrowed: Vec<(&str, &str)> = catalogs
        .iter()
        .map(|(code, text)| (code.as_str(), text.as_str()))
        .collect();
    let output = cap_i18n_build::generate(&borrowed, true)
        .unwrap_or_else(|errors| panic!("{}", errors.join("\n")));
    assert!(output.warnings.is_empty(), "{:?}", output.warnings);
}

#[test]
fn rebranded_catalogs_stay_valid_and_name_sayplay() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../locales");
    let catalogs: Vec<(String, String)> = cap_i18n_build::read_catalogs(&dir)
        .unwrap_or_else(|errors| panic!("{errors:?}"))
        .into_iter()
        .map(|(code, text)| (code, cap_i18n_build::rebrand(&text)))
        .collect();
    let borrowed: Vec<(&str, &str)> = catalogs
        .iter()
        .map(|(code, text)| (code.as_str(), text.as_str()))
        .collect();
    let output = cap_i18n_build::generate(&borrowed, true)
        .unwrap_or_else(|errors| panic!("{}", errors.join("\n")));
    assert!(output.warnings.is_empty(), "{:?}", output.warnings);
    assert!(output.code.contains("\"Quit SayPlay\""));
    for (code, text) in &catalogs {
        for (index, _) in text.match_indices("Cap") {
            let before = text[..index].chars().next_back();
            let after = &text[index + 3..];
            let whole_word = !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                && !after
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            assert!(
                !whole_word || after.starts_with(".so"),
                "{code}: `Cap` left at byte {index}"
            );
        }
    }
}
