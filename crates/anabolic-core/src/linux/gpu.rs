//! GPU usage from the kernel's DRM accounting and, for NVIDIA's own driver,
//! from NVML.
//!
//! - Intel, AMD and other DRM drivers report, for every open GPU context,
//!   how long each engine worked for it (/proc/<pid>/fdinfo). Summed over
//!   the contexts that is the adapter's load; per process, its own.
//! - NVIDIA's driver reports nothing there; its management library does.
//!   A laptop's NVIDIA GPU sleeps while unused, and asking NVML anything
//!   wakes it, so it is asked only while the GPU is awake anyway.
//! - Temperatures, fans and power: the other drivers' hardware monitor,
//!   NVML for NVIDIA's; again only while the GPU is awake.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_uint, c_ulonglong, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::model::ProcKey;
use crate::procfs;
use crate::sensors::{Sensor, SensorKind, Source, hwmon_sources, plain_label};

use super::*;

/// One fdinfo engine: the key after `drm-engine-` (or `drm-cycles-`), and
/// the name Windows' Task Manager would give it.
struct EngineKey {
    key: &'static str,
    name: &'static str,
}

const fn engine(key: &'static str, name: &'static str) -> EngineKey {
    EngineKey { key, name }
}

static I915: [EngineKey; 5] = [
    engine("render", "3D"),
    engine("copy", "Copy"),
    engine("video", "Video"),
    engine("video-enhance", "Video Processing"),
    engine("compute", "Compute"),
];
static XE: [EngineKey; 5] = [
    engine("rcs", "3D"),
    engine("bcs", "Copy"),
    engine("vcs", "Video"),
    engine("vecs", "Video Processing"),
    engine("ccs", "Compute"),
];
static AMDGPU: [EngineKey; 5] = [
    engine("gfx", "3D"),
    engine("compute", "Compute"),
    engine("dma", "Copy"),
    engine("dec", "Video Decode"),
    engine("enc", "Video Encode"),
];
/// What NVML reports: the 3D engine, the encoder and the decoder.
static NVIDIA: [EngineKey; 3] = [
    engine("3d", "3D"),
    engine("enc", "Video Encode"),
    engine("dec", "Video Decode"),
];
static OTHER: [EngineKey; 1] = [engine("render", "3D")];

/// The engines each driver reports, in the order shown.
fn engines_of(driver: &str) -> &'static [EngineKey] {
    match driver {
        "i915" => &I915,
        "xe" => &XE,
        "amdgpu" => &AMDGPU,
        "nvidia" => &NVIDIA,
        _ => &OTHER,
    }
}

struct Adapter {
    /// PCI address, as fdinfo's `drm-pdev` gives it: 0000:00:02.0.
    pdev: String,
    name: Arc<str>,
    driver: String,
    engines: &'static [EngineKey],
    /// Its hardware monitor's readings (not NVIDIA's driver: NVML has them).
    hwmon: Vec<Source>,
}

/// What the per-process thread measured last, per adapter.
#[derive(Clone, Default)]
struct Measured {
    /// Busy share per engine, 0..=100.
    engines: Vec<f32>,
    /// Memory resident for its contexts: in its own memory (VRAM), and in
    /// the system's.
    dedicated: u64,
    shared: u64,
}

pub struct GpuSampler {
    adapters: Arc<Vec<Adapter>>,
    measured: Arc<Mutex<Vec<Measured>>>,
}

impl Default for GpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// A PCI device's name from udev's hardware database.
pub(crate) fn pci_name(pdev: &str) -> Option<String> {
    let data = std::fs::read_to_string(format!("/run/udev/data/+pci:{pdev}")).ok()?;
    let value = |key: &str| {
        data.lines()
            .find_map(|l| l.strip_prefix("E:")?.strip_prefix(key)?.strip_prefix('='))
            .map(str::to_string)
    };
    let model = value("ID_MODEL_FROM_DATABASE")?;
    let vendor = value("ID_VENDOR_FROM_DATABASE").map(|v| {
        v.split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_end_matches(',')
            .to_string()
    });
    Some(match vendor {
        Some(v) if !model.starts_with(&v) => format!("{v} {model}"),
        _ => model,
    })
}

/// Whether a PCI device is powered: a sleeping one must not be woken by
/// asking it how busy it is.
fn awake(pdev: &str) -> bool {
    read(&format!("/sys/bus/pci/devices/{pdev}/power/runtime_status")).is_none_or(|s| s == "active")
}

impl GpuSampler {
    pub fn new() -> Self {
        let mut adapters = Vec::new();
        let mut cards: Vec<String> = std::fs::read_dir("/sys/class/drm")
            .map(|d| {
                d.flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.starts_with("card") && !n.contains('-'))
                    .collect()
            })
            .unwrap_or_default();
        cards.sort_by_key(|c| c[4..].parse::<u32>().unwrap_or(u32::MAX));
        for card in cards {
            let uevent = read(&format!("/sys/class/drm/{card}/device/uevent")).unwrap_or_default();
            let field = |key: &str| {
                uevent
                    .lines()
                    .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                    .map(str::to_string)
            };
            let (Some(pdev), Some(driver)) = (field("PCI_SLOT_NAME"), field("DRIVER")) else {
                continue;
            };
            if adapters.iter().any(|a: &Adapter| a.pdev == pdev) {
                continue;
            }
            let name: Arc<str> =
                Arc::from(pci_name(&pdev).unwrap_or_else(|| format!("{driver} GPU")));
            let hwmon = std::fs::read_dir(format!("/sys/bus/pci/devices/{pdev}/hwmon"))
                .into_iter()
                .flatten()
                .flatten()
                .flat_map(|e| hwmon_sources(&e.path(), &name, None, plain_label))
                .collect();
            adapters.push(Adapter {
                engines: engines_of(&driver),
                name,
                pdev,
                driver,
                hwmon,
            });
        }
        let measured = adapters
            .iter()
            .map(|a| Measured {
                engines: vec![0.0; a.engines.len()],
                ..Default::default()
            })
            .collect();
        Self {
            adapters: Arc::new(adapters),
            measured: Arc::new(Mutex::new(measured)),
        }
    }

    /// Engines per adapter, for sizing history buffers.
    pub fn layout(&self) -> Vec<usize> {
        self.adapters.iter().map(|a| a.engines.len()).collect()
    }

    pub fn sample(&mut self) -> Vec<AdapterGpuStats> {
        let measured = self.measured.lock().map(|m| m.clone()).unwrap_or_default();
        self.adapters
            .iter()
            .zip(measured)
            .map(|(adapter, measured)| {
                let (usages, dedicated, dedicated_total, sensors) = if adapter.driver == "nvidia" {
                    nvidia_sample(&adapter.pdev, &adapter.name)
                        .unwrap_or_else(|| (vec![0.0; adapter.engines.len()], 0, 0, Vec::new()))
                } else {
                    let sensors = if awake(&adapter.pdev) {
                        adapter.hwmon.iter().filter_map(Source::read).collect()
                    } else {
                        Vec::new()
                    };
                    (measured.engines, measured.dedicated, 0, sensors)
                };
                AdapterGpuStats {
                    temperature: sensors
                        .iter()
                        .find(|s| s.kind == SensorKind::Temperature)
                        .map(|s| s.value),
                    sensors,
                    name: adapter.name.clone(),
                    usage: usages.iter().copied().fold(0.0, f32::max),
                    engines: adapter
                        .engines
                        .iter()
                        .zip(&usages)
                        .map(|(e, &usage)| EngineStats {
                            name: Arc::from(e.name),
                            usage,
                        })
                        .collect(),
                    dedicated_used: dedicated,
                    dedicated_total,
                    shared_used: measured.shared,
                    shared_total: 0,
                }
            })
            .collect()
    }
}

/// One open GPU context, from fdinfo.
#[derive(Default)]
struct Client {
    pdev: String,
    /// Per engine key: busy nanoseconds, or busy and total cycles (xe).
    busy: HashMap<String, (u64, u64)>,
    /// How many of each engine the GPU has (a busy time can exceed the
    /// interval when several worked at once).
    capacity: HashMap<String, u64>,
    resident_local: u64,
    resident_system: u64,
}

fn parse_fdinfo(text: &str) -> Option<(u64, Client)> {
    let mut client = Client::default();
    let mut id = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let number = || value.split_ascii_whitespace().next()?.parse::<u64>().ok();
        let kib = || {
            let n = number()?;
            Some(match value.split_ascii_whitespace().nth(1) {
                Some("KiB") => n * 1024,
                Some("MiB") => n * 1024 * 1024,
                _ => n,
            })
        };
        if key == "drm-client-id" {
            id = number();
        } else if key == "drm-pdev" {
            client.pdev = value.to_string();
        } else if let Some(engine) = key.strip_prefix("drm-engine-capacity-") {
            client
                .capacity
                .insert(engine.to_string(), number().unwrap_or(1).max(1));
        } else if let Some(engine) = key.strip_prefix("drm-engine-") {
            client.busy.entry(engine.to_string()).or_default().0 = number().unwrap_or(0);
        } else if let Some(engine) = key.strip_prefix("drm-total-cycles-") {
            client.busy.entry(engine.to_string()).or_default().1 = number().unwrap_or(0);
        } else if let Some(engine) = key.strip_prefix("drm-cycles-") {
            client.busy.entry(engine.to_string()).or_default().0 = number().unwrap_or(0);
        } else if let Some(region) = key.strip_prefix("drm-resident-") {
            let bytes = kib().unwrap_or(0);
            if region.contains("vram") || region.contains("local") {
                client.resident_local += bytes;
            } else {
                client.resident_system += bytes;
            }
        }
    }
    Some((id?, client))
}

/// A process' open DRM files, listed now and then: reading every file
/// name of every process each second would cost more than it tells.
struct Fds {
    starttime: u64,
    drm: Vec<u32>,
    listed: Instant,
}

const RELIST: Duration = Duration::from_secs(5);

fn drm_fds(pid: u32) -> Vec<u32> {
    let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    dir.flatten()
        .filter(|e| {
            std::fs::read_link(e.path())
                .is_ok_and(|t| t.as_os_str().as_encoded_bytes().starts_with(b"/dev/dri/"))
        })
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect()
}

pub(crate) fn spawn_process_sampler(
    sampler: &GpuSampler,
    out: Arc<ArcSwap<GpuProcessMap>>,
    interval_ms: Arc<AtomicU64>,
) {
    if sampler.adapters.is_empty() {
        return;
    }
    let adapters = sampler.adapters.clone();
    let measured = sampler.measured.clone();
    let _ = thread::Builder::new()
        .name("anabolic-gpu".into())
        .spawn(move || {
            crate::hardening::set_current_thread_priority(
                crate::hardening::ThreadPriority::BelowNormal,
            );
            let mut fds: HashMap<u32, Fds> = HashMap::new();
            // (pdev, client id) -> busy per engine, at the last look.
            let mut before: HashMap<(String, u64), HashMap<String, (u64, u64)>> = HashMap::new();
            let mut last = Instant::now();
            loop {
                thread::sleep(Duration::from_millis(
                    interval_ms.load(Ordering::Relaxed).max(250),
                ));
                let now = Instant::now();
                let span = (now - last).as_nanos().max(1) as f64;
                last = now;
                let mut seen: HashMap<(String, u64), HashMap<String, (u64, u64)>> = HashMap::new();
                let mut per_engine = vec![vec![0.0f64; 0]; adapters.len()];
                for (i, a) in adapters.iter().enumerate() {
                    per_engine[i] = vec![0.0; a.engines.len()];
                }
                let mut memory = vec![(0u64, 0u64); adapters.len()];
                let mut shares = GpuProcessMap::new();
                let mut alive = std::collections::HashSet::new();
                for pid in procfs::pids() {
                    let Some(stat) = procfs::stat(pid) else {
                        continue;
                    };
                    if stat.kernel_thread() {
                        continue;
                    }
                    alive.insert(pid);
                    let entry = fds.entry(pid).or_insert_with(|| Fds {
                        starttime: u64::MAX,
                        drm: Vec::new(),
                        listed: now,
                    });
                    if entry.starttime != stat.starttime || now - entry.listed > RELIST {
                        *entry = Fds {
                            starttime: stat.starttime,
                            drm: drm_fds(pid),
                            listed: now,
                        };
                    }
                    let mut share = 0.0f64;
                    for fd in &entry.drm {
                        let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}"))
                        else {
                            continue;
                        };
                        let Some((id, client)) = parse_fdinfo(&text) else {
                            continue;
                        };
                        let key = (client.pdev.clone(), id);
                        // A context shared by several processes (a parent
                        // and the child it forked) counts once.
                        if seen.contains_key(&key) {
                            continue;
                        }
                        let Some(at) = adapters.iter().position(|a| a.pdev == client.pdev) else {
                            seen.insert(key, client.busy);
                            continue;
                        };
                        memory[at].0 += client.resident_local;
                        memory[at].1 += client.resident_system;
                        if let Some(prev) = before.get(&key) {
                            for (e, engine) in adapters[at].engines.iter().enumerate() {
                                let (Some(&(busy, total)), Some(&(busy0, total0))) =
                                    (client.busy.get(engine.key), prev.get(engine.key))
                                else {
                                    continue;
                                };
                                let used = if total > 0 {
                                    // Cycles: busy against the GPU's own clock.
                                    busy.saturating_sub(busy0) as f64
                                        / total.saturating_sub(total0).max(1) as f64
                                } else {
                                    let capacity =
                                        client.capacity.get(engine.key).copied().unwrap_or(1);
                                    busy.saturating_sub(busy0) as f64 / (span * capacity as f64)
                                };
                                per_engine[at][e] += used;
                                share = share.max(used);
                            }
                        }
                        seen.insert(key, client.busy);
                    }
                    if share > 0.0 {
                        shares.insert(
                            ProcKey {
                                pid,
                                create_time: stat.create_time(),
                            },
                            (share * 100.0).min(100.0) as f32,
                        );
                    }
                }
                fds.retain(|pid, _| alive.contains(pid));
                before = seen;
                for (i, adapter) in adapters.iter().enumerate() {
                    if adapter.driver == "nvidia" {
                        for (pid, sm) in nvidia_processes(&adapter.pdev) {
                            if let Some(stat) = procfs::stat(pid) {
                                let key = ProcKey {
                                    pid,
                                    create_time: stat.create_time(),
                                };
                                let s = shares.entry(key).or_default();
                                *s = s.max(sm);
                            }
                        }
                    }
                    if let Ok(mut m) = measured.lock() {
                        m[i] = Measured {
                            engines: per_engine[i]
                                .iter()
                                .map(|u| (u * 100.0).clamp(0.0, 100.0) as f32)
                                .collect(),
                            dedicated: memory[i].0,
                            shared: memory[i].1,
                        };
                    }
                }
                out.store(Arc::new(shares));
            }
        });
}

// NVML, loaded at run time: the library comes with NVIDIA's driver.

type NvmlReturn = c_uint;
type NvmlDevice = *mut c_void;

#[repr(C)]
struct NvmlPciInfo {
    bus_id_legacy: [c_char; 16],
    domain: c_uint,
    bus: c_uint,
    device: c_uint,
    pci_device_id: c_uint,
    pci_sub_system_id: c_uint,
    bus_id: [c_char; 32],
}

#[repr(C)]
#[derive(Default)]
struct NvmlUtilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
#[derive(Default)]
struct NvmlMemory {
    total: c_ulonglong,
    free: c_ulonglong,
    used: c_ulonglong,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NvmlProcessSample {
    pid: c_uint,
    time_stamp: c_ulonglong,
    sm_util: c_uint,
    mem_util: c_uint,
    enc_util: c_uint,
    dec_util: c_uint,
}

type CountFn = unsafe extern "C" fn(*mut c_uint) -> NvmlReturn;
type ByIndexFn = unsafe extern "C" fn(c_uint, *mut NvmlDevice) -> NvmlReturn;
type PciFn = unsafe extern "C" fn(NvmlDevice, *mut NvmlPciInfo) -> NvmlReturn;
type UtilizationFn = unsafe extern "C" fn(NvmlDevice, *mut NvmlUtilization) -> NvmlReturn;
type CodecFn = unsafe extern "C" fn(NvmlDevice, *mut c_uint, *mut c_uint) -> NvmlReturn;
type MemoryFn = unsafe extern "C" fn(NvmlDevice, *mut NvmlMemory) -> NvmlReturn;
/// nvmlDeviceGetTemperature and nvmlDeviceGetTemperatureThreshold: which
/// sensor or threshold, in °C.
type TemperatureFn = unsafe extern "C" fn(NvmlDevice, c_uint, *mut c_uint) -> NvmlReturn;
/// Milliwatts.
type PowerFn = unsafe extern "C" fn(NvmlDevice, *mut c_uint) -> NvmlReturn;
type ProcessesFn = unsafe extern "C" fn(
    NvmlDevice,
    *mut NvmlProcessSample,
    *mut c_uint,
    c_ulonglong,
) -> NvmlReturn;

struct Nvml {
    count: CountFn,
    by_index: ByIndexFn,
    pci: PciFn,
    utilization: UtilizationFn,
    encoder: CodecFn,
    decoder: CodecFn,
    memory: MemoryFn,
    processes: ProcessesFn,
    temperature: Option<TemperatureFn>,
    threshold: Option<TemperatureFn>,
    power: Option<PowerFn>,
}

const NVML_TEMPERATURE_GPU: c_uint = 0;
const NVML_TEMPERATURE_THRESHOLD_SHUTDOWN: c_uint = 0;
const NVML_TEMPERATURE_THRESHOLD_SLOWDOWN: c_uint = 1;

// The library's functions may be called from any thread.
unsafe impl Send for Nvml {}
unsafe impl Sync for Nvml {}

/// NVML, started the first time an NVIDIA GPU is awake to be asked.
fn nvml() -> Option<&'static Nvml> {
    static NVML: OnceLock<Option<Nvml>> = OnceLock::new();
    NVML.get_or_init(|| unsafe {
        let lib = libc::dlopen(
            c"libnvidia-ml.so.1".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        );
        if lib.is_null() {
            return None;
        }
        let sym = |name: &CStr| {
            let f = libc::dlsym(lib, name.as_ptr());
            (!f.is_null()).then_some(f)
        };
        let init = std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> NvmlReturn>(sym(
            c"nvmlInit_v2",
        )?);
        if init() != 0 {
            return None;
        }
        Some(Nvml {
            count: std::mem::transmute::<*mut c_void, CountFn>(sym(c"nvmlDeviceGetCount_v2")?),
            by_index: std::mem::transmute::<*mut c_void, ByIndexFn>(sym(
                c"nvmlDeviceGetHandleByIndex_v2",
            )?),
            pci: std::mem::transmute::<*mut c_void, PciFn>(sym(c"nvmlDeviceGetPciInfo_v3")?),
            utilization: std::mem::transmute::<*mut c_void, UtilizationFn>(sym(
                c"nvmlDeviceGetUtilizationRates",
            )?),
            encoder: std::mem::transmute::<*mut c_void, CodecFn>(sym(
                c"nvmlDeviceGetEncoderUtilization",
            )?),
            decoder: std::mem::transmute::<*mut c_void, CodecFn>(sym(
                c"nvmlDeviceGetDecoderUtilization",
            )?),
            memory: std::mem::transmute::<*mut c_void, MemoryFn>(sym(c"nvmlDeviceGetMemoryInfo")?),
            processes: std::mem::transmute::<*mut c_void, ProcessesFn>(sym(
                c"nvmlDeviceGetProcessUtilization",
            )?),
            temperature: sym(c"nvmlDeviceGetTemperature")
                .map(|f| std::mem::transmute::<*mut c_void, TemperatureFn>(f)),
            threshold: sym(c"nvmlDeviceGetTemperatureThreshold")
                .map(|f| std::mem::transmute::<*mut c_void, TemperatureFn>(f)),
            power: sym(c"nvmlDeviceGetPowerUsage")
                .map(|f| std::mem::transmute::<*mut c_void, PowerFn>(f)),
        })
    })
    .as_ref()
}

/// The NVML device at a PCI address, when it is awake.
fn nvidia_device(pdev: &str) -> Option<(&'static Nvml, NvmlDevice)> {
    if !awake(pdev) {
        return None;
    }
    let nvml = nvml()?;
    let mut count = 0;
    unsafe {
        if (nvml.count)(&mut count) != 0 {
            return None;
        }
        for i in 0..count {
            let mut device: NvmlDevice = std::ptr::null_mut();
            if (nvml.by_index)(i, &mut device) != 0 {
                continue;
            }
            let mut info: NvmlPciInfo = std::mem::zeroed();
            if (nvml.pci)(device, &mut info) != 0 {
                continue;
            }
            // NVML writes the domain with eight digits: 00000000:01:00.0.
            let bus_id = CStr::from_ptr(info.bus_id.as_ptr())
                .to_string_lossy()
                .to_lowercase();
            if bus_id.ends_with(&pdev.to_lowercase()[4..]) {
                return Some((nvml, device));
            }
        }
    }
    None
}

/// (3D, encode, decode) busy shares, used and total memory, temperature
/// and power draw.
fn nvidia_sample(pdev: &str, name: &Arc<str>) -> Option<(Vec<f32>, u64, u64, Vec<Sensor>)> {
    let (nvml, device) = nvidia_device(pdev)?;
    let sensor = |label: &str, kind, value: f32, high, critical| Sensor {
        device: name.clone(),
        label: Arc::from(label),
        kind,
        value,
        high,
        critical,
    };
    let mut sensors = Vec::new();
    unsafe {
        let threshold = |which| {
            let mut t = 0;
            let f = nvml.threshold?;
            (f(device, which, &mut t) == 0 && t > 0).then_some(t as f32)
        };
        let mut t = 0;
        if let Some(f) = nvml.temperature
            && f(device, NVML_TEMPERATURE_GPU, &mut t) == 0
        {
            sensors.push(sensor(
                "Temperature",
                SensorKind::Temperature,
                t as f32,
                threshold(NVML_TEMPERATURE_THRESHOLD_SLOWDOWN),
                threshold(NVML_TEMPERATURE_THRESHOLD_SHUTDOWN),
            ));
        }
        let mut mw = 0;
        if let Some(f) = nvml.power
            && f(device, &mut mw) == 0
        {
            sensors.push(sensor(
                "Power",
                SensorKind::Power,
                mw as f32 / 1000.0,
                None,
                None,
            ));
        }
        let mut rates = NvmlUtilization::default();
        (nvml.utilization)(device, &mut rates);
        let (mut enc, mut dec, mut period) = (0, 0, 0);
        (nvml.encoder)(device, &mut enc, &mut period);
        (nvml.decoder)(device, &mut dec, &mut period);
        let mut memory = NvmlMemory::default();
        (nvml.memory)(device, &mut memory);
        Some((
            vec![rates.gpu as f32, enc as f32, dec as f32],
            memory.used,
            memory.total,
            sensors,
        ))
    }
}

/// The share of the NVIDIA GPU each process used lately.
fn nvidia_processes(pdev: &str) -> Vec<(u32, f32)> {
    let Some((nvml, device)) = nvidia_device(pdev) else {
        return Vec::new();
    };
    // The last few seconds' samples.
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
        .saturating_sub(3_000_000);
    let mut samples = vec![NvmlProcessSample::default(); 256];
    let mut n = samples.len() as c_uint;
    let status = unsafe { (nvml.processes)(device, samples.as_mut_ptr(), &mut n, since) };
    if status != 0 {
        return Vec::new();
    }
    let mut best: HashMap<u32, f32> = HashMap::new();
    for s in &samples[..(n as usize).min(samples.len())] {
        let busy = s.sm_util.max(s.enc_util).max(s.dec_util) as f32;
        let e = best.entry(s.pid).or_default();
        *e = e.max(busy);
    }
    best.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_i915_context() {
        let text = "pos:\t0\nflags:\t02100002\ndrm-driver:\ti915\ndrm-client-id:\t62\n\
                    drm-pdev:\t0000:00:02.0\ndrm-resident-system0:\t281668 KiB\n\
                    drm-engine-render:\t637347665708 ns\ndrm-engine-copy:\t0 ns\n\
                    drm-engine-video:\t5 ns\ndrm-engine-capacity-video:\t2\n";
        let (id, c) = parse_fdinfo(text).unwrap();
        assert_eq!(id, 62);
        assert_eq!(c.pdev, "0000:00:02.0");
        assert_eq!(c.busy["render"].0, 637_347_665_708);
        assert_eq!(c.capacity["video"], 2);
        assert_eq!(c.resident_system, 281_668 * 1024);
        assert_eq!(c.resident_local, 0);
    }

    #[test]
    fn parses_an_xe_context_in_cycles() {
        let text = "drm-client-id:\t7\ndrm-pdev:\t0000:00:02.0\ndrm-cycles-rcs:\t100\n\
                    drm-total-cycles-rcs:\t1000\ndrm-resident-vram0:\t2 MiB\n";
        let (_, c) = parse_fdinfo(text).unwrap();
        assert_eq!(c.busy["rcs"], (100, 1000));
        assert_eq!(c.resident_local, 2 * 1024 * 1024);
    }

    #[test]
    fn lists_the_adapters() {
        let sampler = GpuSampler::new();
        let layout = sampler.layout();
        assert_eq!(layout.len(), sampler.adapters.len());
        assert!(layout.iter().all(|&n| n > 0));
    }

    #[test]
    fn temperatures_only_from_awake_gpus() {
        let mut sampler = GpuSampler::new();
        let stats = sampler.sample();
        for (adapter, stats) in sampler.adapters.iter().zip(&stats) {
            println!(
                "{} ({}, awake {}): {:?}",
                stats.name,
                adapter.driver,
                awake(&adapter.pdev),
                stats.sensors
            );
            if !awake(&adapter.pdev) {
                assert!(stats.sensors.is_empty());
            }
            if let Some(t) = stats.temperature {
                assert!((0.0..150.0).contains(&t));
            }
        }
    }
}
