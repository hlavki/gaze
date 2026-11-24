// SPDX-FileCopyrightText: 2026 Gundu Labs
// SPDX-License-Identifier: GPL-3.0-or-later

//! IR flood illuminators that the kernel exposes as LED class devices.
//!
//! A MIPI IR sensor behind an Intel IPU has no UVC control for its illuminator. The LED is a
//! GPIO that the INT3472 driver registers as `/sys/class/leds/<sensor>::ir_flood_led`, where
//! `<sensor>` is the sensor's ACPI device name with `:` written as `_` (`HIMX1092:00` becomes
//! `HIMX1092_00`). libcamera names the camera after the same device's ACPI path
//! (`\_SB_.LNK0`), which is what ties a configured `libcamera:<name>` source to its LED.

use std::fs;
use std::path::{Path, PathBuf};

const SYSFS_ROOT: &str = "/sys";
const LED_FUNCTION: &str = "::ir_flood_led";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FloodLed {
    name: String,
    dir: PathBuf,
}

impl FloodLed {
    /// The flood LED of the sensor whose ACPI path is `acpi_path`, which is also the name
    /// libcamera gives that camera.
    pub fn for_acpi_path(acpi_path: &str) -> Option<Self> {
        Self::matching(|path| path == acpi_path)
    }

    /// The flood LED of the first sensor whose ACPI path satisfies `matches`.
    pub fn matching(matches: impl Fn(&str) -> bool) -> Option<Self> {
        Self::find_in(Path::new(SYSFS_ROOT), matches)
    }

    fn find_in(sysfs: &Path, matches: impl Fn(&str) -> bool) -> Option<Self> {
        let mut leds: Vec<Self> = fs::read_dir(sysfs.join("class/leds"))
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let sensor = acpi_device_name(name.strip_suffix(LED_FUNCTION)?)?;
                let path =
                    fs::read_to_string(sysfs.join("bus/acpi/devices").join(sensor).join("path"))
                        .ok()?;
                matches(path.trim()).then(|| Self {
                    name,
                    dir: entry.path(),
                })
            })
            .collect();
        leds.sort_by(|a, b| a.name.cmp(&b.name));
        leds.into_iter().next()
    }

    #[cfg(test)]
    pub(crate) fn at(dir: &Path) -> Self {
        Self {
            name: dir.file_name().unwrap().to_str().unwrap().to_string(),
            dir: dir.to_path_buf(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub fn set(&self, on: bool) -> anyhow::Result<()> {
        let level = if on { self.max_brightness() } else { 0 };
        let brightness = self.dir.join("brightness");
        fs::write(&brightness, level.to_string()).map_err(|err| {
            let hint = if err.raw_os_error() == Some(libc::EROFS) {
                // gazed.service sets ProtectKernelTunables, which mounts /sys read-only.
                let device = fs::canonicalize(&self.dir).unwrap_or_else(|_| self.dir.clone());
                format!(
                    "; sysfs is read-only here, add ReadWritePaths={} to gazed.service",
                    device.display()
                )
            } else {
                String::new()
            };
            anyhow::anyhow!("write {level} to {}: {err}{hint}", brightness.display())
        })
    }

    fn max_brightness(&self) -> u32 {
        fs::read_to_string(self.dir.join("max_brightness"))
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .filter(|max| *max > 0)
            .unwrap_or(1)
    }
}

/// Undoes the kernel's LED naming: the instance separator of an ACPI device name is its last
/// `_` there, since hardware IDs never contain one.
fn acpi_device_name(led_sensor: &str) -> Option<String> {
    let (hid, instance) = led_sensor.rsplit_once('_')?;
    (!hid.is_empty() && !instance.is_empty()).then(|| format!("{hid}:{instance}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sysfs_with(leds: &[(&str, &str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (led, acpi_device, acpi_path) in leds {
            let led_dir = root.path().join("class/leds").join(led);
            fs::create_dir_all(&led_dir).unwrap();
            fs::write(led_dir.join("brightness"), "0\n").unwrap();
            fs::write(led_dir.join("max_brightness"), "255\n").unwrap();
            let acpi_dir = root.path().join("bus/acpi/devices").join(acpi_device);
            fs::create_dir_all(&acpi_dir).unwrap();
            fs::write(acpi_dir.join("path"), format!("{acpi_path}\n")).unwrap();
        }
        root
    }

    fn dell_xps() -> tempfile::TempDir {
        sysfs_with(&[
            ("HIMX1092_00::ir_flood_led", "HIMX1092:00", "\\_SB_.LNK0"),
            ("OVTI08F4_00::privacy_led", "OVTI08F4:00", "\\_SB_.LNK1"),
            ("input0::capslock", "PNP0303:00", "\\_SB_.PC00.LPCB.PS2K"),
        ])
    }

    #[test]
    fn the_flood_led_is_found_by_its_sensors_acpi_path() {
        let sysfs = dell_xps();
        let led = FloodLed::find_in(sysfs.path(), |path| path == "\\_SB_.LNK0").unwrap();
        assert_eq!(led.name(), "HIMX1092_00::ir_flood_led");
        assert!(led.path().ends_with("class/leds/HIMX1092_00::ir_flood_led"));
    }

    #[test]
    fn a_sensor_with_only_a_privacy_led_has_no_flood_led() {
        let sysfs = dell_xps();
        assert_eq!(
            FloodLed::find_in(sysfs.path(), |path| path == "\\_SB_.LNK1"),
            None
        );
        assert_eq!(FloodLed::find_in(sysfs.path(), |path| path == "x"), None);
    }

    #[test]
    fn a_flood_led_without_its_acpi_device_is_ignored() {
        let sysfs = dell_xps();
        fs::create_dir_all(sysfs.path().join("class/leds/ABCD0001_00::ir_flood_led")).unwrap();
        assert_eq!(
            FloodLed::find_in(sysfs.path(), |_| true).unwrap().name(),
            "HIMX1092_00::ir_flood_led"
        );
    }

    #[test]
    fn a_machine_without_led_class_devices_has_no_flood_led() {
        let sysfs = tempfile::tempdir().unwrap();
        assert_eq!(FloodLed::find_in(sysfs.path(), |_| true), None);
    }

    #[test]
    fn switching_writes_full_brightness_and_zero() {
        let sysfs = dell_xps();
        let led = FloodLed::find_in(sysfs.path(), |_| true).unwrap();
        let brightness = led.path().join("brightness");

        led.set(true).unwrap();
        assert_eq!(fs::read_to_string(&brightness).unwrap(), "255");
        led.set(false).unwrap();
        assert_eq!(fs::read_to_string(&brightness).unwrap(), "0");
    }

    #[test]
    fn an_unreadable_maximum_still_switches_the_led_on() {
        let sysfs = dell_xps();
        let led = FloodLed::find_in(sysfs.path(), |_| true).unwrap();
        fs::remove_file(led.path().join("max_brightness")).unwrap();

        led.set(true).unwrap();
        assert_eq!(
            fs::read_to_string(led.path().join("brightness")).unwrap(),
            "1"
        );
    }

    #[test]
    fn a_failed_write_names_the_brightness_file() {
        let led = FloodLed {
            name: "GONE0000_00::ir_flood_led".to_string(),
            dir: PathBuf::from("/nonexistent/leds/GONE0000_00::ir_flood_led"),
        };
        let err = led.set(true).unwrap_err().to_string();
        assert!(
            err.contains("GONE0000_00::ir_flood_led/brightness"),
            "{err}"
        );
    }

    #[test]
    fn led_sensor_names_map_back_to_acpi_device_names() {
        assert_eq!(
            acpi_device_name("HIMX1092_00").as_deref(),
            Some("HIMX1092:00")
        );
        assert_eq!(
            acpi_device_name("OVTI08F4_0a").as_deref(),
            Some("OVTI08F4:0a")
        );
        assert_eq!(acpi_device_name("input0"), None);
        assert_eq!(acpi_device_name("_00"), None);
        assert_eq!(acpi_device_name("HIMX1092_"), None);
    }
}
