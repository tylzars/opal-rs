//! Runs against a real disk image when OPAL_COMPOSER_DMG is set.

use std::path::PathBuf;

#[test]
fn extracts_and_verifies_every_asset() {
    let Some(dmg) = std::env::var_os("OPAL_COMPOSER_DMG").map(PathBuf::from) else {
        eprintln!("OPAL_COMPOSER_DMG not set, skipped");
        return;
    };
    let out = std::env::temp_dir().join(format!("opal-assets-test-{}", std::process::id()));
    let mut written = 0;
    opal_assets::extract(&dmg, &out, |event| {
        if let opal_assets::Event::Wrote(..) = event {
            written += 1;
        }
    })
    .expect("extraction failed");
    assert_eq!(written, opal_assets::ASSETS.len());
    for (asset, status) in opal_assets::verify(&out).unwrap() {
        assert!(matches!(status, opal_assets::Status::Ok), "{}", asset.file_name);
    }
    std::fs::remove_dir_all(&out).unwrap();
}
