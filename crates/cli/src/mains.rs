//! Mains frequency guessed from the time zone, for anti-banding.

use opal_dai::message::antibanding;

/// Zones on 60 Hz mains outside the Americas.
const HZ_60: &[&str] = &[
    "Asia/Taipei",
    "Asia/Seoul",
    "Asia/Manila",
    "Asia/Riyadh",
    "Africa/Monrovia",
    "Atlantic/Bermuda",
    "Pacific/Honolulu",
    "Pacific/Guam",
    "Pacific/Saipan",
    "Pacific/Pago_Pago",
    "Pacific/Majuro",
    "Pacific/Palau",
    "US/Hawaii",
    "US/Samoa",
];

/// Zones in the Americas on 50 Hz mains.
const AMERICAS_50: &[&str] = &[
    "America/Argentina/",
    "America/Buenos_Aires",
    "America/Cordoba",
    "America/Santiago",
    "America/Punta_Arenas",
    "America/Montevideo",
    "America/Asuncion",
    "America/La_Paz",
    "America/Cayenne",
    "America/Barbados",
    "America/Martinique",
    "America/Guadeloupe",
    "America/Grenada",
    "America/St_Lucia",
    "America/Dominica",
    "America/St_Vincent",
    "America/Montserrat",
    "America/Marigot",
    "America/St_Barthelemy",
];

/// 50 or 60 for an IANA zone name such as `Europe/Paris`.
pub fn hz_for_zone(zone: &str) -> u32 {
    let americas = ["America/", "US/", "Canada/", "Brazil/", "Mexico/"]
        .iter()
        .any(|p| zone.starts_with(p));
    if (americas && !AMERICAS_50.iter().any(|z| zone.starts_with(z))) || HZ_60.contains(&zone) {
        60
    } else {
        50
    }
}

/// The local zone name, from `TZ`, `/etc/localtime` or `/etc/timezone`.
fn local_zone() -> Option<String> {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if tz.contains('/') {
            return Some(tz.rsplit_once("zoneinfo/").map_or(tz, |(_, z)| z).to_owned());
        }
    }
    if let Ok(target) = std::fs::read_link("/etc/localtime")
        && let Some((_, zone)) = target.to_string_lossy().rsplit_once("zoneinfo/")
    {
        return Some(zone.to_owned());
    }
    std::fs::read_to_string("/etc/timezone")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Anti-banding mode for `antibanding off|50|60|auto`. Auto goes by the time zone, 60 Hz if it is unknown or UTC.
pub fn mode(setting: &str) -> Option<u8> {
    Some(match setting {
        "off" => antibanding::OFF,
        "50" => antibanding::HZ_50,
        "60" => antibanding::HZ_60,
        "auto" => match local_zone()
            .filter(|z| z.contains('/') && !z.starts_with("Etc/"))
            .map_or(60, |z| hz_for_zone(&z))
        {
            50 => antibanding::HZ_50,
            _ => antibanding::HZ_60,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones() {
        assert_eq!(hz_for_zone("America/New_York"), 60);
        assert_eq!(hz_for_zone("America/Sao_Paulo"), 60);
        assert_eq!(hz_for_zone("America/Argentina/Buenos_Aires"), 50);
        assert_eq!(hz_for_zone("America/Santiago"), 50);
        assert_eq!(hz_for_zone("Europe/London"), 50);
        assert_eq!(hz_for_zone("Asia/Tokyo"), 50);
        assert_eq!(hz_for_zone("Asia/Seoul"), 60);
        assert_eq!(hz_for_zone("Pacific/Honolulu"), 60);
        assert_eq!(hz_for_zone("Australia/Sydney"), 50);
    }
}
