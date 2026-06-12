use crate::Profile;

pub const AURA_NAME: &str = "Aura";
pub const COLD_MAGIC: &[u8; 4] = b"AUR0";
pub const WARM_MAGIC: &[u8; 4] = b"AUR1";
pub const GROUPED_HOT_MAGIC: &[u8; 4] = b"AUR2";
pub const ULTRA_HOT_MAGIC: &[u8; 4] = b"AUR3";
pub const FORMAT_VERSION: u16 = 1;

pub fn profile_extension(profile: Profile) -> &'static str {
    match profile {
        Profile::Cold => ".aura.cold",
        Profile::Warm => ".aura.warm",
        Profile::GroupedHot => ".aura.group",
        Profile::UltraHot => ".aura.hot",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_profile_specific() {
        assert_eq!(".aura.cold", profile_extension(Profile::Cold));
        assert_eq!(".aura.hot", profile_extension(Profile::UltraHot));
    }
}
