//! HID access to the BNK8 AI Edition config channel (interface 1).

use anyhow::{Context, Result, anyhow, bail};
use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::frame::REPORT_LEN;

pub const VENDOR_ID: u16 = 0x4249;
pub const PRODUCT_ID: u16 = 0x4E41;
pub const USAGE_PAGE: u16 = 0xFF42;
pub const USAGE: u16 = 0x49;

pub struct Bnk8 {
    dev: HidDevice,
    serial: String,
    fw: u16,
    /// Outgoing sequence counter for protocol exchanges.
    pub(crate) seq: std::cell::Cell<u8>,
}

/// Config-channel interfaces of all attached BNK8 AI Edition devices.
pub fn find(api: &HidApi) -> Vec<DeviceInfo> {
    api.device_list()
        .filter(|d| {
            d.vendor_id() == VENDOR_ID
                && d.product_id() == PRODUCT_ID
                && d.usage_page() == USAGE_PAGE
                && d.usage() == USAGE
        })
        .cloned()
        .collect()
}

pub fn open(serial: Option<&str>) -> Result<Bnk8> {
    let api = HidApi::new().context("initialising hidapi")?;
    let mut found = find(&api);
    if let Some(want) = serial {
        found.retain(|d| d.serial_number() == Some(want));
    }
    let info = match found.as_slice() {
        [] => return Err(not_found(serial)),
        [one] => one,
        many => {
            let serials: Vec<&str> = many
                .iter()
                .map(|d| d.serial_number().unwrap_or("<no serial>"))
                .collect();
            bail!(
                "multiple BNK8 devices; pass --serial (found: {})",
                serials.join(", ")
            );
        }
    };
    let dev = info
        .open_device(&api)
        .with_context(|| format!("opening {}", info.path().to_string_lossy()))?;
    Ok(Bnk8 {
        dev,
        serial: info.serial_number().unwrap_or_default().to_owned(),
        fw: info.release_number(),
        seq: std::cell::Cell::new(0),
    })
}

pub fn not_found(serial: Option<&str>) -> anyhow::Error {
    match serial {
        None => anyhow!("no BNK8 AI Edition found (VID 4249 PID 4E41, usage page FF42)"),
        Some(s) => {
            anyhow!("no BNK8 AI Edition with serial {s} found (VID 4249 PID 4E41, usage page FF42)")
        }
    }
}

/// Formats a USB bcdDevice (`JJ.M.N` BCD) as `major.minor.patch`; `0x0600` -> `6.0.0`.
pub fn format_fw(bcd: u16) -> String {
    let major = (bcd >> 12) * 10 + ((bcd >> 8) & 0xF);
    let minor = (bcd >> 4) & 0xF;
    let patch = bcd & 0xF;
    format!("{major}.{minor}.{patch}")
}

impl Bnk8 {
    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// Firmware version as raw bcdDevice.
    pub fn fw(&self) -> u16 {
        self.fw
    }

    /// Writes one report. The interface has no report IDs, so hidapi needs a leading `0x00`.
    pub fn send(&self, report: &[u8; REPORT_LEN]) -> Result<()> {
        let mut buf = [0u8; REPORT_LEN + 1];
        buf[1..].copy_from_slice(report);
        let n = self.dev.write(&buf).context("writing HID report")?;
        if n != buf.len() {
            bail!("short HID write: {n} of {} bytes", buf.len());
        }
        Ok(())
    }

    /// Reads one report; `None` if nothing arrives within `timeout_ms`.
    pub fn recv(&self, timeout_ms: i32) -> Result<Option<[u8; REPORT_LEN]>> {
        let mut buf = [0u8; REPORT_LEN];
        let n = self
            .dev
            .read_timeout(&mut buf, timeout_ms)
            .context("reading HID report")?;
        match n {
            0 => Ok(None),
            REPORT_LEN => Ok(Some(buf)),
            n => bail!("short HID report: {n} of {REPORT_LEN} bytes"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_fw;

    #[test]
    fn fw_bcd() {
        assert_eq!(format_fw(0x0600), "6.0.0");
        assert_eq!(format_fw(0x1234), "12.3.4");
    }
}
