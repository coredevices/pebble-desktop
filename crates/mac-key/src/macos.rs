use crate::{encryption, Error, HardwareConfig, MacConfig, Result};
use core_foundation::{
    base::{CFType, CFTypeRef, TCFType},
    data::CFData,
    string::{CFString, CFStringRef},
};
use sha2::{Digest, Sha256};
use std::{
    ffi::{c_char, c_void, CStr},
    ptr,
};

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(port: u32, matching: *mut c_void) -> u32;
    fn IORegistryEntryFromPath(port: u32, path: *const c_char) -> u32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFStringRef,
        allocator: *const c_void,
        options: u32,
    ) -> CFTypeRef;
    fn IOObjectRelease(object: u32) -> i32;
    fn IOBSDNameMatching(port: u32, options: u32, name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(port: u32, matching: *mut c_void, iterator: *mut u32) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IORegistryEntryGetParentEntry(entry: u32, plane: *const c_char, parent: *mut u32) -> i32;
}

#[link(name = "DiskArbitration", kind = "framework")]
extern "C" {
    fn DASessionCreate(allocator: *const c_void) -> CFTypeRef;
    fn DADiskCreateFromBSDName(
        allocator: *const c_void,
        session: CFTypeRef,
        name: *const c_char,
    ) -> CFTypeRef;
    fn DADiskCopyDescription(disk: CFTypeRef) -> CFTypeRef;
    static kDADiskDescriptionVolumeUUIDKey: CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDictionaryGetValue(dictionary: CFTypeRef, key: *const c_void) -> CFTypeRef;
    fn CFGetTypeID(value: CFTypeRef) -> usize;
    fn CFUUIDGetTypeID() -> usize;
    fn CFUUIDCreateString(allocator: *const c_void, uuid: CFTypeRef) -> CFStringRef;
}

struct Registry(u32);
impl Drop for Registry {
    fn drop(&mut self) {
        unsafe {
            IOObjectRelease(self.0);
        }
    }
}
impl Registry {
    fn platform() -> Result<Self> {
        let matching = unsafe { IOServiceMatching(c"IOPlatformExpertDevice".as_ptr()) };
        if matching.is_null() {
            return Err(Error::new("Could not find this Mac's hardware"));
        }
        let entry = unsafe { IOServiceGetMatchingService(0, matching) };
        if entry == 0 {
            return Err(Error::new("Could not read this Mac's hardware"));
        }
        Ok(Self(entry))
    }
    fn at(path: &CStr) -> Option<Self> {
        let entry = unsafe { IORegistryEntryFromPath(0, path.as_ptr()) };
        if entry == 0 {
            None
        } else {
            Some(Self(entry))
        }
    }
    fn property(&self, name: &str) -> Option<CFType> {
        let key = CFString::new(name);
        let value = unsafe {
            IORegistryEntryCreateCFProperty(self.0, key.as_concrete_TypeRef(), ptr::null(), 0)
        };
        owned(value)
    }
    fn bytes(&self, name: &str) -> Vec<u8> {
        self.property(name)
            .and_then(|p| p.downcast::<CFData>())
            .map(|d| d.bytes().to_vec())
            .unwrap_or_default()
    }
    fn text(&self, name: &str) -> String {
        let Some(value) = self.property(name) else {
            return String::new();
        };
        if let Some(text) = value.downcast::<CFString>() {
            return text.to_string();
        }
        if let Some(data) = value.downcast::<CFData>() {
            let bytes = data.bytes();
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            return String::from_utf8_lossy(&bytes[..end]).into_owned();
        }
        String::new()
    }
}

fn owned(value: CFTypeRef) -> Option<CFType> {
    if value.is_null() {
        None
    } else {
        Some(unsafe { CFType::wrap_under_create_rule(value) })
    }
}

fn sysctl(name: &CStr) -> Option<Vec<u8>> {
    let mut size = 0;
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            ptr::null_mut(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    } != 0
        || size == 0
        || size > 4096
    {
        return None;
    }
    let mut data = vec![0u8; size];
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            data.as_mut_ptr().cast(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    data.truncate(size);
    Some(data)
}

fn sysctl_text(name: &CStr) -> String {
    sysctl(name)
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.trim_end_matches('\0').to_owned())
        .unwrap_or_default()
}

fn silicon() -> bool {
    sysctl(c"hw.optional.arm64").is_some_and(|bytes| bytes.as_slice() == 1i32.to_ne_bytes())
}

fn permanent_mac_address() -> Option<Vec<u8>> {
    let matching = unsafe { IOBSDNameMatching(0, 0, c"en0".as_ptr()) };
    if matching.is_null() {
        return None;
    }
    let mut iterator = 0;
    if unsafe { IOServiceGetMatchingServices(0, matching, &mut iterator) } != 0 || iterator == 0 {
        return None;
    }
    let iterator = Registry(iterator);
    loop {
        let entry = unsafe { IOIteratorNext(iterator.0) };
        if entry == 0 {
            return None;
        }
        let entry = Registry(entry);
        let mut parent = 0;
        if unsafe { IORegistryEntryGetParentEntry(entry.0, c"IOService".as_ptr(), &mut parent) }
            != 0
            || parent == 0
        {
            continue;
        }
        let parent = Registry(parent);
        let address = parent.bytes("IOMACAddress");
        if address.len() == 6 {
            return Some(address);
        }
    }
}

fn mac_address() -> Result<Vec<u8>> {
    if let Some(address) = permanent_mac_address() {
        return Ok(address);
    }
    struct Interfaces(*mut libc::ifaddrs);
    impl Drop for Interfaces {
        fn drop(&mut self) {
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
    let mut first = ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut first) } != 0 {
        return Err(Error::new("Could not read the primary network adapter"));
    }
    let addresses = Interfaces(first);
    let mut cursor = addresses.0;
    while !cursor.is_null() {
        let interface = unsafe { &*cursor };
        cursor = interface.ifa_next;
        if interface.ifa_addr.is_null() || interface.ifa_name.is_null() {
            continue;
        }
        if unsafe { CStr::from_ptr(interface.ifa_name) } != c"en0" {
            continue;
        }
        let addr = unsafe { &*interface.ifa_addr };
        if i32::from(addr.sa_family) != libc::AF_LINK {
            continue;
        }
        let link = unsafe { &*(interface.ifa_addr as *const libc::sockaddr_dl) };
        let offset = std::mem::offset_of!(libc::sockaddr_dl, sdl_data) + usize::from(link.sdl_nlen);
        if link.sdl_alen != 6 || offset + 6 > usize::from(link.sdl_len) {
            continue;
        }
        return Ok(unsafe {
            std::slice::from_raw_parts(interface.ifa_addr.cast::<u8>().add(offset), 6)
        }
        .to_vec());
    }
    Err(Error::new("Could not read the primary network adapter"))
}

fn root_disk_uuid() -> Result<String> {
    if let Some(chosen) = Registry::at(c"IODeviceTree:/chosen") {
        let boot_uuid = chosen.text("boot-uuid");
        if !boot_uuid.is_empty() {
            return Ok(boot_uuid);
        }
    }
    let missing = || Error::new("Could not read the root disk UUID");
    let session = owned(unsafe { DASessionCreate(ptr::null()) }).ok_or_else(missing)?;
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(c"/".as_ptr(), info.as_mut_ptr()) } != 0 {
        return Err(missing());
    }
    let info = unsafe { info.assume_init() };
    let disk = owned(unsafe {
        DADiskCreateFromBSDName(
            ptr::null(),
            session.as_CFTypeRef(),
            info.f_mntfromname.as_ptr(),
        )
    })
    .ok_or_else(missing)?;
    let description =
        owned(unsafe { DADiskCopyDescription(disk.as_CFTypeRef()) }).ok_or_else(missing)?;
    let uuid = unsafe {
        CFDictionaryGetValue(
            description.as_CFTypeRef(),
            kDADiskDescriptionVolumeUUIDKey.cast(),
        )
    };
    if uuid.is_null() || unsafe { CFGetTypeID(uuid) != CFUUIDGetTypeID() } {
        return Err(missing());
    }
    let text = unsafe { CFUUIDCreateString(ptr::null(), uuid) };
    if text.is_null() {
        return Err(missing());
    }
    Ok(unsafe { CFString::wrap_under_create_rule(text) }.to_string())
}

fn nvram(property: &str) -> Option<String> {
    let result = std::process::Command::new("/usr/sbin/nvram")
        .arg(property)
        .output()
        .ok()?;
    if !result.status.success() {
        return None;
    }
    let text = String::from_utf8(result.stdout).ok()?;
    let (_, value) = text.trim_end_matches(['\n', '\r']).split_once('\t')?;
    Some(value.to_owned())
}

fn percent_bytes(value: &str) -> Option<Vec<u8>> {
    let raw = value.as_bytes();
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < raw.len() {
        if raw[offset] == b'%' {
            let hex = std::str::from_utf8(raw.get(offset + 1..offset + 3)?).ok()?;
            result.push(u8::from_str_radix(hex, 16).ok()?);
            offset += 3;
        } else {
            result.push(raw[offset]);
            offset += 1;
        }
    }
    Some(result)
}

fn uuid_bytes(value: &str) -> Result<Vec<u8>> {
    uuid::Uuid::parse_str(value)
        .map(|u| u.as_bytes().to_vec())
        .map_err(|_| Error::new("A hardware UUID is invalid"))
}

fn enrich(existing: &mut Vec<u8>, source: &[u8]) -> Result<()> {
    if existing.is_empty() {
        *existing = encryption::encrypt(source)?;
    }
    if existing.len() != 17 {
        return Err(Error::new(
            "An encrypted hardware property has an invalid length",
        ));
    }
    Ok(())
}

pub(super) fn extract() -> Result<MacConfig> {
    const PREFIX: &str = "4D1EDE05-38C7-4A6A-9CC6-4BCCA8B38C14";
    let platform = Registry::platform()?;
    let mac = mac_address()?;
    let mut product = platform.text("product-name");
    if product.is_empty() {
        product = sysctl_text(c"hw.model");
    }
    let mut board = platform.text("board-id");
    let chosen = Registry::at(c"IODeviceTree:/chosen");
    if silicon() {
        if let Some(chosen) = &chosen {
            let id = chosen.bytes("board-id");
            if !id.is_empty() {
                board = format!(
                    "Mac-{}",
                    id.iter().map(|b| format!("{b:02x}")).collect::<String>()
                );
            }
        }
    }
    if board.is_empty() {
        board = product.clone();
    }
    let mut rom = platform.bytes(&format!("{PREFIX}:ROM"));
    if rom.is_empty() && silicon() {
        if let Some(chosen) = &chosen {
            let chip = chosen.bytes("unique-chip-id");
            if !chip.is_empty() {
                rom = Sha256::digest(chip)[26..].to_vec();
            }
        }
    }
    let mut mlb = platform.text(&format!("{PREFIX}:MLB"));
    if mlb.is_empty() {
        mlb = platform.text("mlb-serial-number");
    }
    if mlb.is_empty() || rom.is_empty() {
        if let Some(options) = Registry::at(c"IODeviceTree:/options") {
            if mlb.is_empty() {
                mlb = options.text(&format!("{PREFIX}:MLB"));
            }
            if rom.is_empty() {
                rom = options.bytes(&format!("{PREFIX}:ROM"));
            }
        }
    }
    if rom.is_empty() {
        rom = nvram(&format!("{PREFIX}:ROM"))
            .and_then(|s| percent_bytes(&s))
            .unwrap_or_default();
    }
    if mlb.is_empty() {
        mlb = nvram(&format!("{PREFIX}:MLB")).unwrap_or_default();
    }
    let mut hw = HardwareConfig {
        product_name: product,
        io_mac_address: mac,
        platform_serial_number: platform.text("IOPlatformSerialNumber"),
        platform_uuid: platform.text("IOPlatformUUID"),
        root_disk_uuid: root_disk_uuid()?,
        board_id: board,
        os_build_num: sysctl_text(c"kern.osversion"),
        platform_serial_number_enc: platform.bytes("Gq3489ugfi"),
        platform_uuid_enc: platform.bytes("Fyp98tpgj"),
        root_disk_uuid_enc: platform.bytes("kbjfrfpoJU"),
        rom,
        rom_enc: platform.bytes("oycqAZloTNDm"),
        mlb,
        mlb_enc: platform.bytes("abKPld1EcMni"),
    };
    if [
        &hw.product_name,
        &hw.platform_serial_number,
        &hw.board_id,
        &hw.os_build_num,
        &hw.mlb,
    ]
    .iter()
    .any(|s| s.is_empty() || s.len() > 1024)
        || hw.rom.len() != 6
    {
        return Err(Error::new(
            "This Mac is missing required hardware identifiers",
        ));
    }
    enrich(
        &mut hw.platform_serial_number_enc,
        hw.platform_serial_number.as_bytes(),
    )?;
    enrich(&mut hw.platform_uuid_enc, &uuid_bytes(&hw.platform_uuid)?)?;
    enrich(&mut hw.root_disk_uuid_enc, &uuid_bytes(&hw.root_disk_uuid)?)?;
    enrich(&mut hw.rom_enc, &hw.rom)?;
    enrich(&mut hw.mlb_enc, hw.mlb.as_bytes())?;
    let version = sysctl_text(c"kern.osproductversion");
    if version.is_empty() {
        return Err(Error::new("Could not read the macOS version"));
    }
    Ok(MacConfig {
        device_id: hw.platform_uuid.to_uppercase(),
        inner: hw,
        version,
        protocol_version: 1640,
        icloud_ua: "com.apple.iCloudHelper/282 CFNetwork/1408.0.4 Darwin/22.5.0",
        aoskit_version: "com.apple.AOSKit/282 (com.apple.accountsd/113)",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nvram_decoding_and_uuid_validation() {
        assert_eq!(
            percent_bytes("$%1e%eb%08n%ae"),
            Some(vec![36, 30, 235, 8, 110, 174])
        );
        assert_eq!(percent_bytes("%"), None);
        assert_eq!(percent_bytes("%xx"), None);
        assert!(uuid_bytes("not-a-uuid").is_err());
    }
    #[test]
    fn existing_properties_are_preserved_and_malformed_ones_fail() {
        let mut existing = vec![7; 17];
        enrich(&mut existing, b"anything").unwrap();
        assert_eq!(existing, vec![7; 17]);
        assert!(enrich(&mut vec![1; 16], b"anything").is_err());
    }
}
