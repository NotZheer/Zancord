//! Device enumeration and selection (Phase 1C.7): list/select input and output
//! devices on the platform's default host.
//!
//! cpal exposes no stable device ids, so [`AudioDevice::id`] is the device
//! name. On hosts where names are not unique the first match wins.
//!
//! Selection is stateful in [`DeviceManager`]; the stateless free functions
//! (`set_input_device` / `set_output_device`) only validate that an id
//! resolves, so a caller should hold a `DeviceManager` for the selection to
//! stick across stream (re)opens.

use cpal::traits::{DeviceTrait, HostTrait};

use crate::error::{AudioError, Result};

/// A describable audio device on the default host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// Id used to select the device later (its name on the default host).
    pub id: String,
    /// Human-readable device name.
    pub name: String,
    /// Whether this is the host's default device of its direction.
    pub is_default: bool,
}

/// Stateful input/output device selection on the default host.
///
/// Used from the audio thread (streams are opened there), or any thread that
/// wants to enumerate/validate devices without crossing `cpal::Stream`.
pub struct DeviceManager {
    host: cpal::Host,
    input_override: Option<String>,
    output_override: Option<String>,
}

impl DeviceManager {
    /// Attach to the platform default host with no overrides.
    pub fn new() -> Self {
        Self {
            host: cpal::default_host(),
            input_override: None,
            output_override: None,
        }
    }

    /// The host's default input device, if one exists.
    pub fn default_input_device(&self) -> Result<cpal::Device> {
        self.host
            .default_input_device()
            .ok_or_else(|| AudioError::NoDevice("no default input device".to_string()))
    }

    /// The host's default output device, if one exists.
    pub fn default_output_device(&self) -> Result<cpal::Device> {
        self.host
            .default_output_device()
            .ok_or_else(|| AudioError::NoDevice("no default output device".to_string()))
    }

    /// The input device to capture from: the configured override, else the default.
    pub fn input_device(&self) -> Result<cpal::Device> {
        match &self.input_override {
            Some(id) => self.find_input(id),
            None => self.default_input_device(),
        }
    }

    /// The output device to play to: the configured override, else the default.
    pub fn output_device(&self) -> Result<cpal::Device> {
        match &self.output_override {
            Some(id) => self.find_output(id),
            None => self.default_output_device(),
        }
    }

    /// All input devices on the default host, defaults first.
    pub fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
        self.list_devices(self.host.input_devices()?, self.default_input_device().ok())
    }

    /// All output devices on the default host, defaults first.
    pub fn list_output_devices(&self) -> Result<Vec<AudioDevice>> {
        self.list_devices(
            self.host.output_devices()?,
            self.default_output_device().ok(),
        )
    }

    /// Select an input device by id (its name). Errors if it does not resolve.
    pub fn set_input_device(&mut self, id: &str) -> Result<()> {
        self.find_input(id)?;
        self.input_override = Some(id.to_string());
        Ok(())
    }

    /// Select an output device by id (its name). Errors if it does not resolve.
    pub fn set_output_device(&mut self, id: &str) -> Result<()> {
        self.find_output(id)?;
        self.output_override = Some(id.to_string());
        Ok(())
    }

    /// Drop the input override, returning to the default input device.
    pub fn clear_input_device(&mut self) {
        self.input_override = None;
    }

    /// Drop the output override, returning to the default output device.
    pub fn clear_output_device(&mut self) {
        self.output_override = None;
    }

    fn find_input(&self, id: &str) -> Result<cpal::Device> {
        self.find(id, self.host.input_devices()?, "input")
    }

    fn find_output(&self, id: &str) -> Result<cpal::Device> {
        self.find(id, self.host.output_devices()?, "output")
    }

    fn find<I>(&self, id: &str, devices: I, direction: &str) -> Result<cpal::Device>
    where
        I: IntoIterator<Item = cpal::Device>,
    {
        for device in devices {
            if device.name()? == id {
                return Ok(device);
            }
        }
        Err(AudioError::NoDevice(format!(
            "no {direction} device named `{id}`"
        )))
    }

    fn list_devices<I>(&self, devices: I, default: Option<cpal::Device>) -> Result<Vec<AudioDevice>>
    where
        I: IntoIterator<Item = cpal::Device>,
    {
        let default_name = default.and_then(|device| device.name().ok());
        let mut out = Vec::new();
        for device in devices {
            let name = device.name()?;
            let is_default = Some(&name) == default_name.as_ref();
            out.push(AudioDevice {
                id: name.clone(),
                name,
                is_default,
            });
        }
        out.sort_by(|a, b| {
            b.is_default
                .cmp(&a.is_default)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(out)
    }
}

impl Default for DeviceManager {
    fn default() -> Self {
        Self::new()
    }
}

/// List all input devices on the default host, defaults first.
pub fn list_input_devices() -> Result<Vec<AudioDevice>> {
    DeviceManager::new().list_input_devices()
}

/// List all output devices on the default host, defaults first.
pub fn list_output_devices() -> Result<Vec<AudioDevice>> {
    DeviceManager::new().list_output_devices()
}

/// Validate that `id` resolves to an input device on the default host.
///
/// Stateless: hold a [`DeviceManager`] for the selection to persist.
pub fn set_input_device(id: &str) -> Result<()> {
    let mut manager = DeviceManager::new();
    manager.set_input_device(id)
}

/// Validate that `id` resolves to an output device on the default host.
///
/// Stateless: hold a [`DeviceManager`] for the selection to persist.
pub fn set_output_device(id: &str) -> Result<()> {
    let mut manager = DeviceManager::new();
    manager.set_output_device(id)
}
