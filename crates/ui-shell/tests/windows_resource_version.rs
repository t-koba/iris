use std::path::Path;

include!("../build/version_info.rs");

fn checked_in_template() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("windows-resource.rc"))
        .expect("windows-resource.rc must be readable")
}

fn expected_icon_literal() -> String {
    let icon = normalize_icon_path(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons/iris-icon.ico"),
    );
    assert!(icon.is_absolute());
    assert!(icon.ends_with("assets/icons/iris-icon.ico"));
    icon.to_string_lossy().replace('\\', "/")
}

#[test]
fn numeric_version_uses_semver_core() {
    assert_eq!(windows_numeric_version("0.4.0"), Ok([0, 4, 0, 0]));
    assert_eq!(
        windows_numeric_version("1.2.3-beta+build"),
        Ok([1, 2, 3, 0])
    );
}

#[test]
fn numeric_version_rejects_malformed_input() {
    for invalid in [
        "",
        "0.4",
        "0.4.0.0",
        "1.2.x",
        "1..3",
        "v0.4.0",
        " 0.4.0",
        "0.4.0 ",
        "65536.0.0",
        "0.65536.0",
        "0.0.65536",
        "1.2.3-",
        "1.2.3+",
        "1.2.3 beta",
    ] {
        assert!(
            windows_numeric_version(invalid).is_err(),
            "{invalid:?} must be rejected"
        );
    }
}

#[test]
fn template_is_fully_parameterized() {
    let template = checked_in_template();
    for placeholder in VERSION_INFO_PLACEHOLDERS {
        assert!(
            template.contains(placeholder),
            "template must contain {placeholder}"
        );
    }
    assert!(!template.contains("0.4.0"));
    assert!(!template.contains("0,4,0,0"));
    assert!(!template.contains("../../assets/icons/iris-icon.ico"));
}

#[test]
fn rendering_real_template_fills_every_value() {
    let template = checked_in_template();
    let icon_literal = expected_icon_literal();
    let version = "1.2.3-beta+build";
    let numeric = windows_numeric_version(version).unwrap();

    let rendered = render_version_info(&template, &icon_literal, &numeric, version).unwrap();

    assert!(rendered.contains(&format!("IDI_APP_ICON ICON \"{icon_literal}\"")));
    assert!(rendered.contains("FILEVERSION 1,2,3,0"));
    assert!(rendered.contains("PRODUCTVERSION 1,2,3,0"));
    assert!(rendered.contains("VALUE \"FileVersion\", \"1.2.3\\0\""));
    assert!(rendered.contains("VALUE \"ProductVersion\", \"1.2.3-beta+build\\0\""));
    for placeholder in VERSION_INFO_PLACEHOLDERS {
        assert!(!rendered.contains(placeholder));
    }
    assert!(!rendered.contains("@IRIS_"));
}

#[test]
fn rendering_refuses_a_template_with_missing_placeholders() {
    let template = checked_in_template().replace(FILE_VERSION_PLACEHOLDER, "1,2,3,0");
    assert!(render_version_info(
        &template,
        &expected_icon_literal(),
        &[1, 2, 3, 0],
        "1.2.3-beta+build"
    )
    .is_err());
}
