//! Runs against assets/<release>.dmg when it is present.

use std::path::Path;

#[test]
fn extracts_and_verifies_every_asset() {
    let dmg = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets")
        .join(opal_assets::RELEASES[0].dmg_name);
    if !dmg.exists() {
        eprintln!("{} not found, skipped", dmg.display());
        return;
    }
    let out = std::env::temp_dir().join(format!("opal-assets-test-{}", std::process::id()));
    let mut written = 0;
    opal_assets::extract(&dmg, &out, |event| {
        if let opal_assets::Event::Wrote(..) = event {
            written += 1;
        }
    })
    .unwrap();
    assert_eq!(written, opal_assets::ASSETS.len());
    for (asset, status) in opal_assets::verify(&out).unwrap() {
        assert!(matches!(status, opal_assets::Status::Ok), "{}", asset.file_name);
    }
    std::fs::remove_dir_all(&out).unwrap();
}
