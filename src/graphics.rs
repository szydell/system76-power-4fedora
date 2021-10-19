// Copyright 2018-2021 System76 <info@system76.com>
//
// SPDX-License-Identifier: GPL-3.0-only

use crate::{hotplug, module::Module, pci::PciBus};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    iter::FromIterator,
    process::{self, ExitStatus},
};
use sysfs_class::{PciDevice, SysClass};

const MODPROBE_PATH: &str = "/etc/modprobe.d/system76-power.conf";

static MODPROBE_NVIDIA: &[u8] = br#"# Automatically generated by system76-power
options nvidia-drm modeset=1
"#;

static MODPROBE_HYBRID: &[u8] = br#"# Automatically generated by system76-power
blacklist i2c_nvidia_gpu
alias i2c_nvidia_gpu off
options nvidia NVreg_DynamicPowerManagement=0x02
options nvidia-drm modeset=1
"#;

static MODPROBE_COMPUTE: &[u8] = br#"# Automatically generated by system76-power
blacklist i2c_nvidia_gpu
blacklist nvidia-drm
blacklist nvidia-modeset
alias i2c_nvidia_gpu off
alias nvidia-drm off
alias nvidia-modeset off
options nvidia NVreg_DynamicPowerManagement=0x02
"#;

static MODPROBE_INTEGRATED: &[u8] = br#"# Automatically generated by system76-power
blacklist i2c_nvidia_gpu
blacklist nouveau
blacklist nvidia
blacklist nvidia-drm
blacklist nvidia-modeset
alias i2c_nvidia_gpu off
alias nouveau off
alias nvidia off
alias nvidia-drm off
alias nvidia-modeset off
"#;

const PRIME_DISCRETE_PATH: &str = "/etc/prime-discrete";

#[derive(Debug, thiserror::Error)]
pub enum GraphicsDeviceError {
    #[error("failed to execute {} command: {}", cmd, why)]
    Command { cmd: &'static str, why: io::Error },
    #[error("{} in use by {}", func, driver)]
    DeviceInUse { func: String, driver: String },
    #[error("failed to probe driver features: {}", _0)]
    Json(io::Error),
    #[error("failed to open system76-power modprobe file: {}", _0)]
    ModprobeFileOpen(io::Error),
    #[error("failed to write to system76-power modprobe file: {}", _0)]
    ModprobeFileWrite(io::Error),
    #[error("failed to fetch list of active kernel modules: {}", _0)]
    ModulesFetch(io::Error),
    #[error("does not have switchable graphics")]
    NotSwitchable,
    #[error("PCI driver error on {}: {}", device, why)]
    PciDriver { device: String, why: io::Error },
    #[error("failed to get PRIME value: {}", _0)]
    PrimeModeRead(io::Error),
    #[error("failed to set PRIME value: {}", _0)]
    PrimeModeWrite(io::Error),
    #[error("failed to remove PCI device {}: {}", device, why)]
    Remove { device: String, why: io::Error },
    #[error("failed to rescan PCI bus: {}", _0)]
    Rescan(io::Error),
    #[error("failed to read sysfs info: {}", _0)]
    SysFs(io::Error),
    #[error("failed to unbind {} on PCI driver {}: {}", func, driver, why)]
    Unbind { func: String, driver: String, why: io::Error },
    #[error("update-initramfs failed with {} status", _0)]
    UpdateInitramfs(ExitStatus),
    #[error(display = "update-initramfs didn't found tools and failed with {} status", _0)]
    UpdateInitramfsNoTools(ExitStatus),
}

pub struct GraphicsDevice {
    id:        String,
    functions: Vec<PciDevice>,
}

impl GraphicsDevice {
    pub fn new(id: String, functions: Vec<PciDevice>) -> GraphicsDevice {
        GraphicsDevice { id, functions }
    }

    pub fn exists(&self) -> bool { self.functions.iter().any(|func| func.path().exists()) }

    pub unsafe fn unbind(&self) -> Result<(), GraphicsDeviceError> {
        for func in &self.functions {
            if func.path().exists() {
                match func.driver() {
                    Ok(driver) => {
                        log::info!("{}: Unbinding {}", driver.id(), func.id());
                        driver.unbind(func).map_err(|why| GraphicsDeviceError::Unbind {
                            driver: driver.id().to_owned(),
                            func: func.id().to_owned(),
                            why,
                        })?;
                    }
                    Err(why) => match why.kind() {
                        io::ErrorKind::NotFound => (),
                        _ => {
                            return Err(GraphicsDeviceError::PciDriver {
                                device: self.id.clone(),
                                why,
                            })
                        }
                    },
                }
            }
        }

        Ok(())
    }

    pub unsafe fn remove(&self) -> Result<(), GraphicsDeviceError> {
        for func in &self.functions {
            if func.path().exists() {
                match func.driver() {
                    Ok(driver) => {
                        log::error!("{}: in use by {}", func.id(), driver.id());
                        return Err(GraphicsDeviceError::DeviceInUse {
                            func:   func.id().to_owned(),
                            driver: driver.id().to_owned(),
                        });
                    }
                    Err(why) => match why.kind() {
                        io::ErrorKind::NotFound => {
                            log::info!("{}: Removing", func.id());
                            func.remove().map_err(|why| GraphicsDeviceError::Remove {
                                device: self.id.clone(),
                                why,
                            })?;
                        }
                        _ => {
                            return Err(GraphicsDeviceError::PciDriver {
                                device: self.id.clone(),
                                why,
                            })
                        }
                    },
                }
            } else {
                log::warn!("{}: Already removed", func.id());
            }
        }

        Ok(())
    }
}

// supported-gpus.json
#[derive(Serialize, Deserialize, Debug)]
struct NvidiaDevice {
    devid:        String,
    subdeviceid:  Option<String>,
    subvendorid:  Option<String>,
    name:         String,
    legacybranch: Option<String>,
    features:     Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct SupportedGpus {
    chips: Vec<NvidiaDevice>,
}

pub struct Graphics {
    pub bus:    PciBus,
    pub amd:    Vec<GraphicsDevice>,
    pub intel:  Vec<GraphicsDevice>,
    pub nvidia: Vec<GraphicsDevice>,
    pub other:  Vec<GraphicsDevice>,
}

impl Graphics {
    pub fn new() -> io::Result<Graphics> {
        let bus = PciBus::new()?;

        log::info!("Rescanning PCI bus");
        bus.rescan()?;

        let devs = PciDevice::all()?;

        let functions = |parent: &PciDevice| -> Vec<PciDevice> {
            let mut functions = Vec::new();
            if let Some(parent_slot) = parent.id().split('.').next() {
                for func in &devs {
                    if let Some(func_slot) = func.id().split('.').next() {
                        if func_slot == parent_slot {
                            log::info!("{}: Function for {}", func.id(), parent.id());
                            functions.push(func.clone());
                        }
                    }
                }
            }
            functions
        };

        let mut amd = Vec::new();
        let mut intel = Vec::new();
        let mut nvidia = Vec::new();
        let mut other = Vec::new();
        for dev in &devs {
            let c = dev.class()?;
            if let 0x03 = (c >> 16) & 0xFF {
                match dev.vendor()? {
                    0x1002 => {
                        log::info!("{}: AMD graphics", dev.id());
                        amd.push(GraphicsDevice::new(dev.id().to_owned(), functions(dev)));
                    }
                    0x10DE => {
                        log::info!("{}: NVIDIA graphics", dev.id());
                        nvidia.push(GraphicsDevice::new(dev.id().to_owned(), functions(dev)));
                    }
                    0x8086 => {
                        log::info!("{}: Intel graphics", dev.id());
                        intel.push(GraphicsDevice::new(dev.id().to_owned(), functions(dev)));
                    }
                    vendor => {
                        log::info!("{}: Other({:X}) graphics", dev.id(), vendor);
                        other.push(GraphicsDevice::new(dev.id().to_owned(), functions(dev)));
                    }
                }
            }
        }

        Ok(Graphics { bus, amd, intel, nvidia, other })
    }

    pub fn can_switch(&self) -> bool {
        !self.nvidia.is_empty() && (!self.intel.is_empty() || !self.amd.is_empty())
    }

    pub fn get_external_displays_require_dgpu(&self) -> Result<bool, GraphicsDeviceError> {
        self.switchable_or_fail()?;

        let model = fs::read_to_string("/sys/class/dmi/id/product_version")
            .map_err(GraphicsDeviceError::SysFs)?;

        Ok(hotplug::REQUIRES_NVIDIA.contains(&model.trim()))
    }

    fn nvidia_version(&self) -> Result<String, GraphicsDeviceError> {
        fs::read_to_string("/sys/module/nvidia/version")
            .map_err(GraphicsDeviceError::SysFs)
            .map(|s| s.trim().to_string())
    }

    fn get_nvidia_device_id(&self) -> Result<u32, GraphicsDeviceError> {
        let device = format!("/sys/bus/pci/devices/{}/device", self.nvidia[0].id);
        let id = fs::read_to_string(device).map_err(GraphicsDeviceError::SysFs)?;
        let id = id.trim_start_matches("0x").trim();
        u32::from_str_radix(id, 16).map_err(|e| {
            GraphicsDeviceError::SysFs(io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
        })
    }

    fn get_nvidia_device(&self, id: u32) -> Result<NvidiaDevice, GraphicsDeviceError> {
        let version = self.nvidia_version()?;
        let major =
            version.split('.').next().unwrap_or_default().parse::<u32>().unwrap_or_default();

        let supported_gpus = format!("/usr/share/doc/nvidia-driver-{}/supported-gpus.json", major);
        let raw = fs::read_to_string(supported_gpus).map_err(GraphicsDeviceError::Json)?;
        let gpus: SupportedGpus = serde_json::from_str(&raw).map_err(|e| {
            GraphicsDeviceError::Json(io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
        })?;

        // There may be multiple entries that share the same device ID.
        for dev in gpus.chips {
            let did = dev.devid.trim_start_matches("0x").trim();
            let did = u32::from_str_radix(did, 16).unwrap_or_default();
            if did == id {
                return Ok(dev);
            }
        }

        Err(GraphicsDeviceError::Json(io::Error::new(
            io::ErrorKind::NotFound,
            "GPU device not found",
        )))
    }

    fn gpu_supports_runtimepm(&self) -> Result<bool, GraphicsDeviceError> {
        let id = self.get_nvidia_device_id()?;
        let dev = self.get_nvidia_device(id)?;
        log::info!("Device 0x{:04} features: {:?}", id, dev.features);
        Ok(dev.features.contains(&"runtimepm".to_string()))
    }

    pub fn get_default_graphics(&self) -> Result<String, GraphicsDeviceError> {
        // Models that support runtimepm, but should not use hybrid graphics
        const DEFAULT_INTEGRATED: &[&str] = &[];

        self.switchable_or_fail()?;

        let product = fs::read_to_string("/sys/class/dmi/id/product_version")
            .map_err(GraphicsDeviceError::SysFs)
            .map(|s| s.trim().to_string())?;
        let blacklisted = DEFAULT_INTEGRATED.contains(&product.as_str());

        // If the NVIDIA device is not on the bus or the drivers are not
        // loaded, then assume runtimepm is not supported.
        let runtimepm = self.gpu_supports_runtimepm().unwrap_or_default();

        // Only default to hybrid on System76 models
        let vendor = fs::read_to_string("/sys/class/dmi/id/sys_vendor")
            .map_err(GraphicsDeviceError::SysFs)
            .map(|s| s.trim().to_string())?;

        if vendor != "System76" {
            Ok("nvidia".to_string())
        } else if runtimepm && !blacklisted {
            Ok("hybrid".to_string())
        } else {
            Ok("integrated".to_string())
        }
    }

    fn get_prime_discrete() -> Result<String, GraphicsDeviceError> {
        fs::read_to_string(PRIME_DISCRETE_PATH)
            .map_err(GraphicsDeviceError::PrimeModeRead)
            .map(|mode| mode.trim().to_owned())
    }

    fn set_prime_discrete(mode: &str) -> Result<(), GraphicsDeviceError> {
        fs::write(PRIME_DISCRETE_PATH, mode).map_err(GraphicsDeviceError::PrimeModeWrite)
    }

    pub fn get_vendor(&self) -> Result<String, GraphicsDeviceError> {
        let modules = Module::all().map_err(GraphicsDeviceError::ModulesFetch)?;
        let vendor =
            if modules.iter().any(|module| module.name == "nouveau" || module.name == "nvidia") {
                let mode = match Self::get_prime_discrete() {
                    Ok(m) => m,
                    Err(_) => "nvidia".to_string(),
                };

                if mode == "on-demand" {
                    "hybrid".to_string()
                } else if mode == "off" {
                    "compute".to_string()
                } else {
                    "nvidia".to_string()
                }
            } else {
                "integrated".to_string()
            };

        Ok(vendor)
    }

    pub fn set_vendor(&self, vendor: &str) -> Result<(), GraphicsDeviceError> {
        self.switchable_or_fail()?;

        let mode = if vendor == "hybrid" {
            "on-demand\n"
        } else if vendor == "nvidia" {
            "on\n"
        } else {
            // Integrated or Compute
            "off\n"
        };

        log::info!("Setting {} to {}", PRIME_DISCRETE_PATH, mode);
        Self::set_prime_discrete(mode)?;

        {
            log::info!("Creating {}", MODPROBE_PATH);

            let mut file = fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(MODPROBE_PATH)
                .map_err(GraphicsDeviceError::ModprobeFileOpen)?;

            let text = if vendor == "hybrid" {
                MODPROBE_HYBRID
            } else if vendor == "compute" {
                MODPROBE_COMPUTE
            } else if vendor == "nvidia" {
                MODPROBE_NVIDIA
            } else {
                MODPROBE_INTEGRATED
            };

            file.write_all(text)
                .and_then(|_| file.sync_all())
                .map_err(GraphicsDeviceError::ModprobeFileWrite)?;
        }

        const SYSTEMCTL_CMD: &str = "systemctl";

        let action = if vendor == "nvidia" {
            log::info!("Enabling nvidia-fallback.service");
            "enable"
        } else {
            log::info!("Disabling nvidia-fallback.service");
            "disable"
        };

        let status = process::Command::new(SYSTEMCTL_CMD)
            .arg(action)
            .arg("nvidia-fallback.service")
            .status()
            .map_err(|why| GraphicsDeviceError::Command { cmd: SYSTEMCTL_CMD, why })?;

        if !status.success() {
            // Error is ignored in case this service is removed
            log::warn!(
                "systemctl: failed with {} (not an error if service does not exist!)",
                status
            );
        }

        log::info!("Updating initramfs");
        

        const COMMAND_CMD: &str = "command";
        const UPDATE_DRACUT_CMD: &str = "dracut";
        const UPDATE_INITRAMFS_CMD: &str = "update-initramfs";

        if process::Command::new(COMMAND_CMD).arg("-v").arg(UPDATE_DRACUT_CMD).stdout(process::Stdio::null()).status().map_err(|why| GraphicsDeviceError::Command { cmd: UPDATE_DRACUT_CMD, why })?.success() {

            let status = process::Command::new(UPDATE_DRACUT_CMD)
                .arg("--force")
                .status()
                .map_err(|why| GraphicsDeviceError::Command { cmd: UPDATE_DRACUT_CMD, why })?;
            if ! status.success() {
                return Err(GraphicsDeviceError::UpdateInitramfs(status));
            }

            
        } else {

            let status = process::Command::new(UPDATE_INITRAMFS_CMD)
                .arg("-u")
                .status()
                .map_err(|why| GraphicsDeviceError::Command { cmd: UPDATE_INITRAMFS_CMD, why })?;
        
            if ! status.success() {
                return Err(GraphicsDeviceError::UpdateInitramfs(status));
            }

        }

        Ok(())
    }

    pub fn get_power(&self) -> Result<bool, GraphicsDeviceError> {
        self.switchable_or_fail()?;
        Ok(self.nvidia.iter().any(GraphicsDevice::exists))
    }

    pub fn set_power(&self, power: bool) -> Result<(), GraphicsDeviceError> {
        self.switchable_or_fail()?;

        if power {
            log::info!("Enabling graphics power");
            self.bus.rescan().map_err(GraphicsDeviceError::Rescan)?;
        } else {
            log::info!("Disabling graphics power");

            // TODO: Don't allow turning off power if nvidia_drm modeset is enabled

            unsafe {
                // Unbind NVIDIA graphics devices and their functions
                let unbinds = self.nvidia.iter().map(|dev| dev.unbind());

                // Remove NVIDIA graphics devices and their functions
                let removes = self.nvidia.iter().map(|dev| dev.remove());

                Result::from_iter(unbinds.chain(removes))?;
            }
        }

        Ok(())
    }

    pub fn auto_power(&self) -> Result<(), GraphicsDeviceError> {
        let vendor = self.get_vendor()?;
        self.set_power(vendor != "integrated")
    }

    fn switchable_or_fail(&self) -> Result<(), GraphicsDeviceError> {
        if self.can_switch() {
            Ok(())
        } else {
            Err(GraphicsDeviceError::NotSwitchable)
        }
    }
}
