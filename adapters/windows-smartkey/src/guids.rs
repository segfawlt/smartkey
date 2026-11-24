pub const CLSID_SMARTKEY_TEXT_SERVICE_STR: &str = "{8F4E5B3A-6D21-4C9E-9A74-3E2B1C8F05D1}";
pub const GUID_SMARTKEY_PROFILE_EN_US_STR: &str = "{C3A92E14-7B58-4F62-B81D-94E05A2C67F2}";

pub const LANGID_EN_US: u16 = 0x0409;
pub const SMARTKEY_DISPLAY_NAME: &str = "SmartKey Autocorrect";

#[cfg(windows)]
pub const CLSID_SMARTKEY_TEXT_SERVICE: windows_core::GUID =
    windows_core::GUID::from_u128(0x8F4E_5B3A_6D21_4C9E_9A74_3E2B_1C8F_05D1);

#[cfg(windows)]
pub const GUID_SMARTKEY_PROFILE_EN_US: windows_core::GUID =
    windows_core::GUID::from_u128(0xC3A9_2E14_7B58_4F62_B81D_94E0_5A2C_67F2);
