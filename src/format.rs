use crate::Profile;

pub const AURA_NAME: &str = "Aura";
pub const AURA_MAGIC: &[u8; 4] = b"AURA";
pub const INGEST_MAGIC: &[u8; 4] = b"AURA";
pub const AURA0_MAGIC: &[u8; 4] = b"AUR0";
pub const AURA1_MAGIC: &[u8; 4] = b"AUR1";
pub const SEAL_MAGIC: &[u8; 8] = b"sealed:)";
pub const FORMAT_VERSION: u16 = 2;

pub fn profile_extension(profile: Profile) -> &'static str {
    match profile {
        Profile::Ingest => ".aura",
        Profile::Aura0 => ".aura0",
        Profile::Aura1 => ".aura1",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_public_profiles() {
        assert_eq!(".aura", profile_extension(Profile::Ingest));
        assert_eq!(".aura0", profile_extension(Profile::Aura0));
        assert_eq!(".aura1", profile_extension(Profile::Aura1));
    }
}
