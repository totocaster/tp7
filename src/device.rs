use nusb::{DeviceInfo, MaybeFuture};
use serde::Serialize;

use crate::output::AppError;

pub const TP7_VENDOR_ID: u16 = 0x2367;
pub const TP7_MTP_PRODUCT_ID: u16 = 0x0019;
pub const TP7_AUDIO_MIDI_PRODUCT_ID: u16 = 0x8019;

pub fn is_tp7_product_id(product_id: u16) -> bool {
    matches!(product_id, TP7_MTP_PRODUCT_ID | TP7_AUDIO_MIDI_PRODUCT_ID)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UsbMode {
    AudioMidi,
    Mtp,
    MassStorage,
    Mixed,
    Unknown,
}

impl std::fmt::Display for UsbMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            UsbMode::AudioMidi => "audio-midi",
            UsbMode::Mtp => "mtp",
            UsbMode::MassStorage => "mass-storage",
            UsbMode::Mixed => "mixed",
            UsbMode::Unknown => "unknown",
        };
        f.write_str(label)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Tp7Device {
    pub vendor_id: u16,
    pub product_id: u16,
    pub vendor_id_hex: String,
    pub product_id_hex: String,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial_number: Option<String>,
    pub mode: UsbMode,
    pub speed: Option<String>,
    pub usb_version: String,
    pub device_version: Option<String>,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub bus_id: Option<String>,
    pub device_address: Option<u8>,
    pub port_chain: Vec<u8>,
    pub location_id: Option<String>,
    pub registry_entry_id: Option<String>,
    pub interfaces: Vec<UsbInterface>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsbInterface {
    pub number: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub class_name: String,
    pub name: Option<String>,
}

pub fn list_tp7_devices() -> Result<Vec<Tp7Device>, AppError> {
    let devices = nusb::list_devices()
        .wait()
        .map_err(|error| AppError::UsbEnumeration {
            message: error.to_string(),
        })?;

    Ok(devices
        .filter(|device| {
            device.vendor_id() == TP7_VENDOR_ID && is_tp7_product_id(device.product_id())
        })
        .map(Tp7Device::from)
        .collect())
}

pub fn filter_by_serial(
    devices: Vec<Tp7Device>,
    serial: Option<&str>,
) -> Result<Vec<Tp7Device>, AppError> {
    let Some(serial) = serial else {
        return Ok(devices);
    };

    let filtered = devices
        .into_iter()
        .filter(|device| device.serial_number.as_deref() == Some(serial))
        .collect::<Vec<_>>();

    if filtered.is_empty() {
        return Err(AppError::DeviceNotFound {
            serial: serial.to_string(),
        });
    }

    Ok(filtered)
}

pub fn select_one_device(
    devices: Vec<Tp7Device>,
    serial: Option<&str>,
) -> Result<Tp7Device, AppError> {
    let devices = filter_by_serial(devices, serial)?;

    match devices.len() {
        0 => Err(AppError::NoDevices),
        1 => Ok(devices.into_iter().next().expect("one device exists")),
        count => Err(AppError::MultipleDevices { count }),
    }
}

impl From<DeviceInfo> for Tp7Device {
    fn from(device: DeviceInfo) -> Self {
        let interfaces = device
            .interfaces()
            .map(|interface| UsbInterface {
                number: interface.interface_number(),
                class: interface.class(),
                subclass: interface.subclass(),
                protocol: interface.protocol(),
                class_name: usb_class_name(interface.class()).to_string(),
                name: interface.interface_string().map(str::to_owned),
            })
            .collect::<Vec<_>>();

        let mode = infer_usb_mode(&interfaces);

        Self {
            vendor_id: device.vendor_id(),
            product_id: device.product_id(),
            vendor_id_hex: format_hex16(device.vendor_id()),
            product_id_hex: format_hex16(device.product_id()),
            manufacturer: device.manufacturer_string().map(str::to_owned),
            product: device.product_string().map(str::to_owned),
            serial_number: device.serial_number().map(str::to_owned),
            mode,
            speed: speed_label(&device),
            usb_version: bcd_version(device.usb_version()),
            device_version: device_version(&device),
            class: device.class(),
            subclass: device.subclass(),
            protocol: device.protocol(),
            bus_id: bus_id(&device),
            device_address: device_address(&device),
            port_chain: port_chain(&device),
            location_id: location_id(&device),
            registry_entry_id: registry_entry_id(&device),
            interfaces,
        }
    }
}

pub fn infer_usb_mode(interfaces: &[UsbInterface]) -> UsbMode {
    let has_mtp = interfaces.iter().any(is_mtp_interface);
    let has_mass_storage = interfaces.iter().any(|interface| interface.class == 0x08);
    let has_audio = interfaces.iter().any(|interface| interface.class == 0x01);
    let has_midi = interfaces
        .iter()
        .any(|interface| interface.class == 0x01 && interface.subclass == 0x03);

    match (has_mtp, has_mass_storage, has_audio || has_midi) {
        (true, false, false) => UsbMode::Mtp,
        (false, true, false) => UsbMode::MassStorage,
        (false, false, true) => UsbMode::AudioMidi,
        (true, _, _) | (_, true, true) => UsbMode::Mixed,
        _ => UsbMode::Unknown,
    }
}

fn is_mtp_interface(interface: &UsbInterface) -> bool {
    let name = interface.name.as_deref().unwrap_or("").to_lowercase();

    (interface.class == 0x06 && interface.subclass == 0x01)
        || (interface.class == 0xff
            && interface.subclass == 0x01
            && interface.protocol == 0x01
            && (name.contains("mtp") || name.contains("tetp")))
}

pub fn interface_summary(interfaces: &[UsbInterface]) -> String {
    if interfaces.is_empty() {
        return "none".to_string();
    }

    interfaces
        .iter()
        .map(|interface| {
            let name = interface
                .name
                .as_deref()
                .unwrap_or(interface.class_name.as_str());
            format!(
                "{}:{}:{} {}",
                interface.class, interface.subclass, interface.protocol, name
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn usb_class_name(class: u8) -> &'static str {
    match class {
        0x01 => "audio",
        0x02 => "communications",
        0x03 => "hid",
        0x06 => "still-image",
        0x08 => "mass-storage",
        0x09 => "hub",
        0x0a => "cdc-data",
        0x0e => "video",
        0xe0 => "wireless-controller",
        0xef => "miscellaneous",
        0xff => "vendor-specific",
        _ => "unknown",
    }
}

fn format_hex16(value: u16) -> String {
    format!("0x{value:04x}")
}

fn format_hex32(value: u32) -> String {
    format!("0x{value:08x}")
}

fn format_hex64(value: u64) -> String {
    format!("0x{value:016x}")
}

fn bcd_version(value: u16) -> String {
    format!(
        "{}.{}.{}",
        (value >> 8) & 0xff,
        (value >> 4) & 0x0f,
        value & 0x0f
    )
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn device_version(device: &DeviceInfo) -> Option<String> {
    Some(bcd_version(device.device_version()))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn device_version(_device: &DeviceInfo) -> Option<String> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn bus_id(device: &DeviceInfo) -> Option<String> {
    Some(device.bus_id().to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn bus_id(_device: &DeviceInfo) -> Option<String> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn device_address(device: &DeviceInfo) -> Option<u8> {
    Some(device.device_address())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn device_address(_device: &DeviceInfo) -> Option<u8> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn port_chain(device: &DeviceInfo) -> Vec<u8> {
    device.port_chain().to_vec()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn port_chain(_device: &DeviceInfo) -> Vec<u8> {
    Vec::new()
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn speed_label(device: &DeviceInfo) -> Option<String> {
    device
        .speed()
        .map(|speed| format!("{speed:?}").to_lowercase())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn speed_label(_device: &DeviceInfo) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn location_id(device: &DeviceInfo) -> Option<String> {
    Some(format_hex32(device.location_id()))
}

#[cfg(not(target_os = "macos"))]
fn location_id(_device: &DeviceInfo) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn registry_entry_id(device: &DeviceInfo) -> Option<String> {
    Some(format_hex64(device.registry_entry_id()))
}

#[cfg(not(target_os = "macos"))]
fn registry_entry_id(_device: &DeviceInfo) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interface(class: u8, subclass: u8, protocol: u8) -> UsbInterface {
        UsbInterface {
            number: 0,
            class,
            subclass,
            protocol,
            class_name: usb_class_name(class).to_string(),
            name: None,
        }
    }

    fn tp7_device(serial_number: Option<&str>) -> Tp7Device {
        Tp7Device {
            vendor_id: TP7_VENDOR_ID,
            product_id: TP7_AUDIO_MIDI_PRODUCT_ID,
            vendor_id_hex: "0x2367".to_string(),
            product_id_hex: "0x8019".to_string(),
            manufacturer: Some("teenage engineering".to_string()),
            product: Some("TP-7".to_string()),
            serial_number: serial_number.map(str::to_string),
            mode: UsbMode::AudioMidi,
            speed: Some("high".to_string()),
            usb_version: "2.0.0".to_string(),
            device_version: Some("2.5.7".to_string()),
            class: 0,
            subclass: 0,
            protocol: 0,
            bus_id: Some("01".to_string()),
            device_address: Some(1),
            port_chain: vec![1],
            location_id: Some("0x01100000".to_string()),
            registry_entry_id: Some("0x00000001000b6528".to_string()),
            interfaces: vec![],
        }
    }

    #[test]
    fn infers_audio_midi_mode() {
        let interfaces = vec![interface(0x01, 0x01, 0), interface(0x01, 0x03, 0)];

        assert_eq!(infer_usb_mode(&interfaces), UsbMode::AudioMidi);
    }

    #[test]
    fn recognizes_audio_midi_and_mtp_product_ids() {
        assert!(is_tp7_product_id(TP7_AUDIO_MIDI_PRODUCT_ID));
        assert!(is_tp7_product_id(TP7_MTP_PRODUCT_ID));
        assert!(!is_tp7_product_id(0x1234));
    }

    #[test]
    fn infers_mtp_mode() {
        let interfaces = vec![interface(0x06, 0x01, 0x01)];

        assert_eq!(infer_usb_mode(&interfaces), UsbMode::Mtp);
    }

    #[test]
    fn infers_tp7_vendor_mtp_mode() {
        let mut mtp = interface(0xff, 0x01, 0x01);
        mtp.name = Some("MTP TETP interface".to_string());

        assert_eq!(infer_usb_mode(&[mtp]), UsbMode::Mtp);
    }

    #[test]
    fn infers_mixed_mode() {
        let interfaces = vec![interface(0x01, 0x03, 0), interface(0x06, 0x01, 0x01)];

        assert_eq!(infer_usb_mode(&interfaces), UsbMode::Mixed);
    }

    #[test]
    fn filters_by_serial() {
        let devices = vec![tp7_device(Some("A")), tp7_device(Some("B"))];

        let filtered = filter_by_serial(devices, Some("B")).unwrap();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].serial_number.as_deref(), Some("B"));
    }

    #[test]
    fn returns_error_for_missing_serial() {
        let devices = vec![tp7_device(Some("A"))];

        let error = filter_by_serial(devices, Some("B")).unwrap_err();

        assert!(matches!(error, AppError::DeviceNotFound { .. }));
    }

    #[test]
    fn selects_one_device() {
        let device = select_one_device(vec![tp7_device(Some("A"))], None).unwrap();

        assert_eq!(device.serial_number.as_deref(), Some("A"));
    }

    #[test]
    fn requires_serial_for_multiple_devices() {
        let devices = vec![tp7_device(Some("A")), tp7_device(Some("B"))];

        let error = select_one_device(devices, None).unwrap_err();

        assert!(matches!(error, AppError::MultipleDevices { count: 2 }));
    }
}
