//! Local, read-only Mac activation-key extraction for the future Tauri host.
//! The returned key contains sensitive hardware identifiers; never log it.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Serialize;
use std::fmt;

#[cfg(target_os = "macos")]
mod encryption;
#[cfg(target_os = "macos")]
mod macos;

pub struct Error(String);
impl Error {
    fn new(message: &str) -> Self {
        Self(message.to_owned())
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

// Field order matches the released Corten GUI and CLI, including its base64 output.
#[derive(Serialize)]
struct HardwareConfig {
    product_name: String,
    io_mac_address: Vec<u8>,
    platform_serial_number: String,
    platform_uuid: String,
    root_disk_uuid: String,
    board_id: String,
    os_build_num: String,
    platform_serial_number_enc: Vec<u8>,
    platform_uuid_enc: Vec<u8>,
    root_disk_uuid_enc: Vec<u8>,
    rom: Vec<u8>,
    rom_enc: Vec<u8>,
    mlb: String,
    mlb_enc: Vec<u8>,
}

#[derive(Serialize)]
struct MacConfig {
    inner: HardwareConfig,
    version: String,
    protocol_version: u32,
    device_id: String,
    icloud_ua: &'static str,
    aoskit_version: &'static str,
}

fn encode(config: &MacConfig) -> Result<String> {
    let json =
        serde_json::to_string(config).map_err(|_| Error::new("Could not encode the Mac key"))?;
    // Go's JSON encoder uses these escapes in the released reference CLI.
    let json = json
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    Ok(STANDARD.encode(json.as_bytes()))
}

/// Returns Corten's exact base64 key, without a trailing newline. Does not write files or access the network.
/// Invoke from Tauri's blocking worker pool so native reads do not block the UI thread.
pub fn extract_key() -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        encode(&macos::extract()?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(Error::new(
            "Mac activation keys can only be extracted on macOS",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_matches_corten_order_and_escaping() {
        let config = MacConfig {
            inner: HardwareConfig {
                product_name: "Mac\"<&>\n".into(),
                io_mac_address: vec![0, 128, 255],
                platform_serial_number: "serial".into(),
                platform_uuid: "uuid".into(),
                root_disk_uuid: "disk".into(),
                board_id: "board".into(),
                os_build_num: "build".into(),
                platform_serial_number_enc: vec![],
                platform_uuid_enc: vec![],
                root_disk_uuid_enc: vec![],
                rom: vec![],
                rom_enc: vec![],
                mlb: "mlb".into(),
                mlb_enc: vec![],
            },
            version: "1.2.3".into(),
            protocol_version: 1640,
            device_id: "DEVICE".into(),
            icloud_ua: "ua",
            aoskit_version: "kit",
        };
        let actual = encode(&config).unwrap();
        let expected = concat!(
            r#"{"inner":{"product_name":"Mac\"\u003c\u0026\u003e\n","io_mac_address":[0,128,255],"#,
            r#""platform_serial_number":"serial","platform_uuid":"uuid","root_disk_uuid":"disk","#,
            r#""board_id":"board","os_build_num":"build","platform_serial_number_enc":[],"#,
            r#""platform_uuid_enc":[],"root_disk_uuid_enc":[],"rom":[],"rom_enc":[],"mlb":"mlb","mlb_enc":[]},"#,
            r#""version":"1.2.3","protocol_version":1640,"device_id":"DEVICE","icloud_ua":"ua","aoskit_version":"kit"}"#,
        );
        assert_eq!(STANDARD.decode(&actual).unwrap(), expected.as_bytes());
        assert!(!actual.ends_with('\n'));
    }
}
