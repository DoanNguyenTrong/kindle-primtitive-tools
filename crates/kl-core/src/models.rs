//! The Kindle models Primitive can run on, and how to recognise them.
//!
//! The list is the owner's choice of jailbreakable Kindles (docs/supported-devices.md in the umbrella repo). A model is
//! recognised from its platform codename (the `stinger_<codename>` in /etc/version.txt) when that codename is known;
//! otherwise the owner chooses it. Only the Oasis 3 is verified on a device. Install rules are by architecture alone:
//! a model that is not verified installs anything that has its architecture, with a warning.

/// One model in the supported list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    /// Stable id, stored in `settings.txt` as `device_model=<id>`.
    pub id: &'static str,
    /// The name shown to the owner.
    pub name: &'static str,
    /// The catalog's name for the model (`tested_on` in a listing).
    pub catalog_name: &'static str,
    /// Platform codenames that identify the model from /etc/version.txt. Empty when the codename is not known yet.
    pub codenames: &'static [&'static str],
    /// The architecture its jailbreak builds use.
    pub arch: &'static str,
    /// The firmware range the jailbreak works on (information only).
    pub firmware: &'static str,
    /// Whether PrimitiveOS has been run on this model.
    pub verified: bool,
}

/// The supported models. Order is the order of the choice list.
pub const MODELS: &[Model] = &[
    Model { id: "oasis3", name: "Kindle Oasis 3", catalog_name: "KindleOasis3", codenames: &["zelda"], arch: "armhf", firmware: "5.16.3 to 5.19.5", verified: true },
    Model { id: "paperwhite5", name: "Kindle Paperwhite 5", catalog_name: "KindlePaperwhite5", codenames: &[], arch: "armhf", firmware: "5.16.3 to 5.19.5", verified: false },
    Model { id: "paperwhite5se", name: "Kindle Paperwhite 5 Signature Edition", catalog_name: "KindlePaperwhite5SE", codenames: &[], arch: "armhf", firmware: "5.16.3 to 5.19.5", verified: false },
    Model { id: "paperwhite6", name: "Kindle Paperwhite 6", catalog_name: "KindlePaperwhite6", codenames: &[], arch: "armhf", firmware: "5.16.3 to 5.19.5", verified: false },
    Model { id: "paperwhite4", name: "Kindle Paperwhite 4", catalog_name: "KindlePaperwhite4", codenames: &[], arch: "armhf", firmware: "5.16.3 to 5.19.5", verified: false },
    Model { id: "kindle11-2022", name: "Kindle 11 (2022)", catalog_name: "Kindle11", codenames: &[], arch: "armhf", firmware: "5.16.3 to 5.19.5", verified: false },
    Model { id: "kindle11-2024", name: "Kindle 11 (2024)", catalog_name: "Kindle11_2024", codenames: &[], arch: "armhf", firmware: "5.16.3 to 5.19.5", verified: false },
    Model { id: "kindle10", name: "Kindle 10 (2019, Basic)", catalog_name: "Kindle10", codenames: &[], arch: "armhf", firmware: "5.16.3 to 5.19.5", verified: false },
    Model { id: "colorsoft", name: "Kindle Colorsoft", catalog_name: "KindleColorsoft", codenames: &[], arch: "armhf", firmware: "5.16.3 to 5.19.5", verified: false },
    Model { id: "scribe2022", name: "Kindle Scribe (2022)", catalog_name: "KindleScribe", codenames: &[], arch: "armhf", firmware: "5.16.3 to 5.19.5", verified: false },
    Model { id: "scribe2024", name: "Kindle Scribe (2024)", catalog_name: "KindleScribe2024", codenames: &[], arch: "armhf", firmware: "5.16.3 to 5.19.5", verified: false },
];

/// The model whose platform codename is `codename`, if one is known.
pub fn model_for_codename(codename: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.codenames.iter().any(|c| c.eq_ignore_ascii_case(codename)))
}

/// The model with `id`.
pub fn model_by_id(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}

/// The platform codename in a `System Software Version` line, such as `…_stinger_zelda-443455` (`zelda`).
pub fn codename_from_version(text: &str) -> Option<String> {
    let start = text.find("stinger_")? + "stinger_".len();
    let name: String = text[start..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

/// The series number the stock Device Info shows, built from the system version string: the build number after the last
/// `-` followed by the leading number before the first `-` (`025-juno_…_zelda-443455` gives `443455025`).
pub fn series_number(version_text: &str) -> Option<String> {
    let token = version_text.split_once("Version: ").map_or(version_text, |(_, rest)| rest).split_whitespace().next()?;
    let (lead, build) = (token.split('-').next()?, token.rsplit('-').next()?);
    let lead: String = lead.chars().take_while(|c| c.is_ascii_digit()).collect();
    let build: String = build.chars().take_while(|c| c.is_ascii_digit()).collect();
    (!lead.is_empty() && !build.is_empty()).then(|| format!("{build}{lead}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_oasis_3_is_recognised_from_its_codename() {
        let text = "System Software Version: 025-juno_1802_stinger_zelda-443455\n";
        assert_eq!(codename_from_version(text).as_deref(), Some("zelda"));
        assert_eq!(model_for_codename("zelda").map(|m| m.id), Some("oasis3"));
    }

    #[test]
    fn the_series_number_is_the_build_then_the_lead() {
        let text = "System Software Version: 025-juno_1802_stinger_zelda-443455 Thu Apr 10 02:26:31 UTC 2025 com.lab126";
        assert_eq!(series_number(text).as_deref(), Some("443455025"));
        assert_eq!(series_number("nothing"), None);
    }

    #[test]
    fn an_unknown_codename_is_not_guessed() {
        assert!(model_for_codename("rex").is_none());
        assert_eq!(codename_from_version("no version here"), None);
    }

    #[test]
    fn every_model_has_an_id_an_arch_and_a_unique_id() {
        let mut ids: Vec<&str> = MODELS.iter().map(|m| m.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), MODELS.len());
        assert!(MODELS.iter().all(|m| !m.arch.is_empty() && !m.catalog_name.is_empty()));
        assert_eq!(MODELS.iter().filter(|m| m.verified).count(), 1, "only the Oasis 3 is verified");
    }
}
