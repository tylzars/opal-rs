//! Expected camera files and where they are in the disk image.

pub struct Release {
    pub label: &'static str,
    pub dmg_name: &'static str,
    pub dmg_sha256: &'static str,
}

pub const RELEASES: &[Release] = &[Release {
    label: "Opal Composer 2.0 (build 24)",
    dmg_name: "Opal_Composer_2.0.0_24.dmg",
    dmg_sha256: "4eaa1225a203b057dbabaa7b17d7bbff91512cac32e942359b651dbef06928b3",
}];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Myriad X application image
    DeviceFirmware,
    /// Myriad X neural network
    NeuralNetwork,
    /// Sensor tuning
    SensorTuning,
}

#[derive(Clone, Copy, Debug)]
pub enum Source {
    /// Path in the volume
    File(&'static str),
    /// Member of an xz tar embedded in a file
    EmbeddedArchive {
        container: &'static str,
        member: &'static str,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct Asset {
    pub file_name: &'static str,
    pub kind: Kind,
    pub source: Source,
    pub size: u64,
    pub sha256: &'static str,
}

#[cfg(test)]
const RESOURCES: &str = "Opal Composer.app/Contents/XPCServices/OpalCameraDeviceService.xpc/Contents/Resources";
const FIRMWARE_CONTAINER: &str = "Opal Composer.app/Contents/Frameworks/libdepthai-core.dylib";
const FIRMWARE_MEMBER: &str = "depthai-device-openvino-2021.4-e2929eb3993d759114d7084074a0eec845c12b45.cmd";

macro_rules! resource {
    ($name:literal) => {
        Source::File(concat!(
            "Opal Composer.app/Contents/XPCServices/OpalCameraDeviceService.xpc/Contents/Resources/",
            $name
        ))
    };
}

pub const ASSETS: &[Asset] = &[
    Asset {
        file_name: FIRMWARE_MEMBER,
        kind: Kind::DeviceFirmware,
        source: Source::EmbeddedArchive {
            container: FIRMWARE_CONTAINER,
            member: FIRMWARE_MEMBER,
        },
        size: 25_206_008,
        sha256: "0f1f74f8d6097fd0d9f3f9885d922df28057e1af41f57dc23c64f22e91e3c9f0",
    },
    Asset {
        file_name: "2684560952.tlv",
        kind: Kind::NeuralNetwork,
        source: resource!("2684560952.tlv"),
        size: 7_808,
        sha256: "6f2878177b3a5072b85368e551a69c94c8bc7aa42de247944f0bbe1458775658",
    },
    Asset {
        file_name: "3ea9231461.tlv",
        kind: Kind::NeuralNetwork,
        source: resource!("3ea9231461.tlv"),
        size: 5_544_832,
        sha256: "32ba8ffd7112951b76c8e0008e9f37163720ee5d8b9264c952032cd879e42eb2",
    },
    Asset {
        file_name: "49559974de.tlv",
        kind: Kind::NeuralNetwork,
        source: resource!("49559974de.tlv"),
        size: 938_752,
        sha256: "62c85740e1032278a8d6a6bde31950470c65140ca2efecb7a9621ea228de17bf",
    },
    Asset {
        file_name: "6127be7a1f.tlv",
        kind: Kind::NeuralNetwork,
        source: resource!("6127be7a1f.tlv"),
        size: 768_576,
        sha256: "349a5a772969efdcfd5dff90ef52b9d4dc1a40a27edd6ee7a67c1c0e4ffc2b1d",
    },
    Asset {
        file_name: "76fe11746e.tlv",
        kind: Kind::NeuralNetwork,
        source: resource!("76fe11746e.tlv"),
        size: 4_662_784,
        sha256: "cede38f7e4373f74fe30f8f2d4ce83fabf8e256b8b8d4d7b317e5c01814d287c",
    },
    Asset {
        file_name: "dd0bad95b2.tlv",
        kind: Kind::NeuralNetwork,
        source: resource!("dd0bad95b2.tlv"),
        size: 1_344_256,
        sha256: "3d59700823e1c20862079fc9531d0ae589790c7c0a112488ec27bded24722c5b",
    },
    Asset {
        file_name: "Opal_v0.12.bin",
        kind: Kind::SensorTuning,
        source: resource!("Opal_v0.12.bin"),
        size: 197_092,
        sha256: "8c723046f982093b473345ebf02d38a652995b78bd985a0cc224a6cd3a0d22e1",
    },
    Asset {
        file_name: "Opal582_v2.bin",
        kind: Kind::SensorTuning,
        source: resource!("Opal582_v2.bin"),
        size: 195_916,
        sha256: "f41c80032c262d69631be3a0f41264138a335e1948882b3c1bf5d9270a6e95d6",
    },
];

pub fn asset(file_name: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.file_name == file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_macro_matches_constant() {
        let Source::File(path) = resource!("x") else {
            unreachable!()
        };
        assert_eq!(path, format!("{RESOURCES}/x"));
    }

    #[test]
    fn manifest_is_well_formed() {
        for (i, a) in ASSETS.iter().enumerate() {
            assert_eq!(a.sha256.len(), 64, "{}", a.file_name);
            assert!(
                a.sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
            assert!(
                ASSETS[..i].iter().all(|b| b.file_name != a.file_name),
                "duplicate {}",
                a.file_name
            );
        }
    }
}
