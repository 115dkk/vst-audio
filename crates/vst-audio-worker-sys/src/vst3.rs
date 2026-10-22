// The generated C++ enum constants are i32 on Windows and u32 on Unix.
#![allow(clippy::unnecessary_cast)]
#![allow(unsafe_code)]
//! VST 3 isolation. Lifecycle/controller/editor/state run on the creating main
//! thread. Only process/setProcessing run on the sole audio thread. getState is
//! permitted in the Processing state; setState uses a nonblocking admission gate.
//! <https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical+Documentation/API+Documentation/Index.html>
use crate::{
    editor::EditorWindow,
    library::Library,
    plugin::PluginError,
    vst3_support::{Changes, Edits, Frame, Handler, Host, Stream},
};
use std::{
    ffi::{c_char, c_void},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use vst3::{ComPtr, ComWrapper, Interface, Steinberg::Vst::*, Steinberg::*};

fn check(result: tresult, operation: &str) -> Result<(), PluginError> {
    if result == kResultOk {
        Ok(())
    } else {
        Err(PluginError(format!("VST 3 {operation}: {result}")))
    }
}
fn text(bytes: &[c_char]) -> String {
    let bytes: Vec<_> = bytes
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub id: [u8; 16],
    pub name: String,
    pub vendor: String,
    pub category: String,
}
#[derive(Clone, Debug)]
pub struct Info {
    pub class: ClassInfo,
    pub input: u64,
    pub output: u64,
    pub inputs: u16,
    pub outputs: u16,
    pub latency: u32,
    pub input_buses: Vec<vst_audio_protocol::BusInfo>,
    pub output_buses: Vec<vst_audio_protocol::BusInfo>,
}
#[derive(Debug, Default)]
pub struct Events {
    pub params: bool,
    pub latency: Option<u32>,
    pub closed: bool,
    pub fault: Option<&'static str>,
}
struct ModuleLibrary {
    library: Library,
    exit: Option<unsafe extern "system" fn() -> bool>,
}
impl Drop for ModuleLibrary {
    fn drop(&mut self) {
        if let Some(exit) = self.exit {
            // SAFETY: module still loaded, all factory and instance COM references already released.
            let _ = unsafe { exit() };
        }
    }
}
// SAFETY: module ownership may follow the last processor reference; no calls are
// made through Library concurrently. Host teardown joins audio before main drops.
unsafe impl Send for ModuleLibrary {}
// SAFETY: shared lifetime keeper only; Library is never mutated after loading.
unsafe impl Sync for ModuleLibrary {}
pub struct Vst3Module {
    factory: ComPtr<IPluginFactory>,
    host: ComPtr<FUnknown>,
    #[cfg(target_os = "linux")]
    run_loop: Rc<crate::linux_runloop::RunLoop>,
    library: Arc<ModuleLibrary>,
}
impl std::fmt::Debug for Vst3Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Module").finish_non_exhaustive()
    }
}
fn binary(path: &Path) -> Result<PathBuf, PluginError> {
    if !path.is_dir() {
        return Ok(path.to_owned());
    }
    #[cfg(target_os = "macos")]
    {
        Ok(path.to_owned())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let name = path
            .file_name()
            .ok_or_else(|| PluginError("invalid VST3 bundle".into()))?;
        #[cfg(all(windows, target_arch = "aarch64"))]
        let architectures = ["arm64-win", "arm64ec-win"];
        #[cfg(all(windows, target_arch = "x86_64"))]
        let architectures = ["x86_64-win"];
        #[cfg(all(windows, target_arch = "x86"))]
        let architectures = ["x86-win"];
        #[cfg(all(target_os = "linux", target_arch = "x86"))]
        let architectures = ["i386-linux"];
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let architectures = ["x86_64-linux"];
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        let architectures = ["aarch64-linux"];
        #[cfg(windows)]
        let filename = name.to_os_string();
        #[cfg(target_os = "linux")]
        let filename = Path::new(name).with_extension("so").into_os_string();
        architectures
            .iter()
            .map(|arch| path.join("Contents").join(arch).join(&filename))
            .find(|file| file.is_file())
            .ok_or_else(|| PluginError("VST3 bundle has no binary for this architecture".into()))
    }
}
impl Vst3Module {
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        let library = Library::load(&binary(path)?).map_err(|e| PluginError(e.to_string()))?;
        #[cfg(windows)]
        let exit_name = c"ExitDll";
        #[cfg(target_os = "linux")]
        let exit_name = c"ModuleExit";
        #[cfg(target_os = "macos")]
        let exit_name = c"bundleExit";
        let exit = library.symbol(exit_name).map(|symbol| {
            // SAFETY: SDK Windows module exit export has this exact ABI.
            unsafe {
                std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    unsafe extern "system" fn() -> bool,
                >(symbol)
            }
        });
        #[cfg(windows)]
        if let Some(symbol) = library.symbol(c"InitDll") {
            // SAFETY: SDK Windows module initialization export has this exact ABI.
            let init: unsafe extern "system" fn() -> bool = unsafe { std::mem::transmute(symbol) };
            // SAFETY: DLL is loaded; initialization precedes all factory calls.
            if !unsafe { init() } {
                return Err(PluginError("VST 3 InitDll failed".into()));
            }
        }
        #[cfg(unix)]
        {
            #[cfg(target_os = "linux")]
            let entry_name = c"ModuleEntry";
            #[cfg(target_os = "macos")]
            let entry_name = c"bundleEntry";
            if let Some(symbol) = library.symbol(entry_name) {
                // SAFETY: VST3 platform entry accepts the retained dlopen/CFBundle context.
                let entry: unsafe extern "system" fn(*mut c_void) -> bool =
                    unsafe { std::mem::transmute(symbol) };
                // SAFETY: native module is loaded and remains owned through module exit.
                if !unsafe { entry(library.module_context()) } {
                    return Err(PluginError("VST3 module initialization failed".into()));
                }
            }
        }
        let library = Arc::new(ModuleLibrary { library, exit });
        let symbol = library
            .library
            .symbol(c"GetPluginFactory")
            .ok_or_else(|| PluginError("missing GetPluginFactory".into()))?;
        // SAFETY: SDK factory export uses system ABI and returns an owned factory reference.
        let entry: unsafe extern "system" fn() -> *mut IPluginFactory =
            unsafe { std::mem::transmute(symbol) };
        // SAFETY: initialized module, factory entry point validated by export name.
        let pointer = unsafe { entry() };
        // SAFETY: adopt the reference returned by GetPluginFactory, checking null.
        let factory = unsafe { ComPtr::from_raw(pointer) }
            .ok_or_else(|| PluginError("null VST 3 factory".into()))?;
        let host_object = Host::new();
        #[cfg(target_os = "linux")]
        let run_loop = host_object.run_loop.clone();
        let host = ComWrapper::new(host_object)
            .to_com_ptr::<FUnknown>()
            .expect("host interface");
        if let Some(factory3) = factory.cast::<IPluginFactory3>() {
            // SAFETY: main thread, live factory and retained host context.
            let _ = unsafe { factory3.setHostContext(host.as_ptr()) };
        }
        Ok(Self {
            factory,
            host,
            #[cfg(target_os = "linux")]
            run_loop,
            library,
        })
    }
    pub fn scan(&self) -> Result<Vec<ClassInfo>, PluginError> {
        // SAFETY: plain SDK struct containing only integers and fixed character arrays.
        let mut vendor: PFactoryInfo = unsafe { std::mem::zeroed() };
        check(
            // SAFETY: main-thread live factory and correctly sized output structure.
            unsafe { self.factory.getFactoryInfo(&mut vendor) },
            "getFactoryInfo",
        )?;
        // SAFETY: initialized factory, called on main thread.
        let count = unsafe { self.factory.countClasses() };
        if !(0..=4096).contains(&count) {
            return Err(PluginError("invalid VST 3 class count".into()));
        }
        let factory2 = self.factory.cast::<IPluginFactory2>();
        let mut classes = Vec::new();
        for index in 0..count {
            // SAFETY: plain fixed-array/integer output struct has a valid all-zero representation.
            let mut info: PClassInfo = unsafe { std::mem::zeroed() };
            check(
                // SAFETY: valid enumerated index and writable class-info output.
                unsafe { self.factory.getClassInfo(index, &mut info) },
                "getClassInfo",
            )?;
            if text(&info.category) != "Audio Module Class" {
                continue;
            }
            let mut class = ClassInfo {
                id: info.cid.map(|b| b as u8),
                name: text(&info.name),
                vendor: text(&vendor.vendor),
                category: String::new(),
            };
            if let Some(factory2) = &factory2 {
                // SAFETY: fixed-array/integer class-info output has a valid zero representation.
                let mut info: PClassInfo2 = unsafe { std::mem::zeroed() };
                // SAFETY: factory2 was queried successfully, index and output are valid.
                if unsafe { factory2.getClassInfo2(index, &mut info) } == kResultOk {
                    class.name = text(&info.name);
                    if !text(&info.vendor).is_empty() {
                        class.vendor = text(&info.vendor);
                    }
                    class.category = text(&info.subCategories);
                }
            }
            classes.push(class);
        }
        Ok(classes)
    }
    fn create<I: Interface>(&self, id: [u8; 16]) -> Result<ComPtr<I>, PluginError> {
        let mut object = std::ptr::null_mut();
        check(
            // SAFETY: 16-byte IDs match SDK TUID byte order; factory writes an owned requested interface.
            unsafe {
                self.factory
                    .createInstance(id.as_ptr().cast(), I::IID.as_ptr().cast(), &mut object)
            },
            "createInstance",
        )?;
        // SAFETY: successful factory call transfers one owned reference of the requested interface.
        unsafe { ComPtr::from_raw(object.cast()) }
            .ok_or_else(|| PluginError("null VST 3 instance".into()))
    }
}
struct Shared {
    processor: ComPtr<IAudioProcessor>,
    admitted: AtomicBool,
    busy: AtomicBool,
    active: AtomicBool,
    closed: AtomicBool,
    edits: Arc<Edits>,
    _library: Arc<ModuleLibrary>,
}
struct Admission<'a>(&'a Shared);
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.admitted.store(true, Ordering::SeqCst);
    }
}
impl Shared {
    fn control(&self) -> Admission<'_> {
        self.admitted.store(false, Ordering::SeqCst);
        while self.busy.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Admission(self)
    }
}
struct Editor {
    scale: f32,
    view: ComPtr<IPlugView>,
    _frame: ComPtr<IPlugFrame>,
    window: Rc<EditorWindow>,
}
impl Drop for Editor {
    fn drop(&mut self) {
        // SAFETY: attached view, main-thread teardown before window destruction.
        let _ = unsafe { self.view.removed() };
        // SAFETY: detach frame reference to avoid retaining the host window.
        let _ = unsafe { self.view.setFrame(std::ptr::null_mut()) };
    }
}
pub struct Vst3Plugin {
    controller: Option<ComPtr<IEditController>>,
    separate: bool,
    component_initialized: bool,
    controller_initialized: bool,
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    editor: Option<Editor>,
    handler: ComPtr<IComponentHandler>,
    shared: Arc<Shared>,
    // Child interfaces must be released before the component (DPF enforces this).
    component: ComPtr<IComponent>,
    info: Info,
    module: Vst3Module,
    closed_editor: bool,
    processor_taken: bool,
    max_frames: usize,
    setup: Option<vst_audio_protocol::ProcessingSetup>,
    capabilities: vst_audio_protocol::Capabilities,
    _main: std::marker::PhantomData<Rc<()>>,
}
impl std::fmt::Debug for Vst3Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Plugin")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}
impl Vst3Plugin {
    pub fn load(path: &Path, class_id: Option<[u8; 16]>) -> Result<Self, PluginError> {
        let module = Vst3Module::load(path)?;
        let class = module
            .scan()?
            .into_iter()
            .find(|class| class_id.is_none_or(|id| class.id == id))
            .ok_or_else(|| PluginError("VST 3 audio effect class not found".into()))?;
        let component = module.create::<IComponent>(class.id)?;
        let processor = component
            .cast::<IAudioProcessor>()
            .ok_or_else(|| PluginError("missing IAudioProcessor".into()))?;
        let edits = Arc::new(Edits::new());
        let handler = ComWrapper::new(Handler(edits.clone()))
            .to_com_ptr()
            .expect("handler interface");
        let shared = Arc::new(Shared {
            processor,
            admitted: AtomicBool::new(true),
            busy: AtomicBool::new(false),
            active: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            edits,
            _library: module.library.clone(),
        });
        let mut plugin = Self {
            component,
            controller: None,
            separate: false,
            component_initialized: false,
            controller_initialized: false,
            connection: None,
            editor: None,
            handler,
            shared,
            info: Info {
                class,
                input: 0,
                output: 0,
                inputs: 0,
                outputs: 0,
                latency: 0,
                input_buses: Vec::new(),
                output_buses: Vec::new(),
            },
            module,
            closed_editor: false,
            processor_taken: false,
            max_frames: 0,
            setup: None,
            capabilities: Default::default(),
            _main: std::marker::PhantomData,
        };
        check(
            // SAFETY: first initialization on main thread with retained host context.
            unsafe { plugin.component.initialize(plugin.module.host.as_ptr()) },
            "component initialize",
        )?;
        plugin.component_initialized = true;
        plugin.controller = plugin.component.cast::<IEditController>();
        if plugin.controller.is_none() {
            let mut id = [0; 16];
            // SAFETY: initialized component, writable controller class ID output.
            if unsafe { plugin.component.getControllerClassId(&mut id) } == kResultOk {
                let controller = plugin
                    .module
                    .create::<IEditController>(id.map(|b| b as u8))?;
                plugin.separate = true;
                plugin.controller = Some(controller);
                check(
                    // SAFETY: distinct controller initialized once on main thread.
                    unsafe {
                        plugin
                            .controller
                            .as_ref()
                            .expect("controller")
                            .initialize(plugin.module.host.as_ptr())
                    },
                    "controller initialize",
                )?;
                plugin.controller_initialized = true;
            }
        }
        if let Some(controller) = &plugin.controller {
            check(
                // SAFETY: main-thread controller and retained host handler.
                unsafe { controller.setComponentHandler(plugin.handler.as_ptr()) },
                "setComponentHandler",
            )?;
            if plugin.separate
                && let (Some(a), Some(b)) = (
                    plugin.component.cast::<IConnectionPoint>(),
                    controller.cast::<IConnectionPoint>(),
                )
            {
                // SAFETY: both connection endpoints are live and main-thread initialized.
                check(unsafe { a.connect(b.as_ptr()) }, "component connect")?;
                plugin.connection = Some((a, b));
                let (a, b) = plugin.connection.as_ref().expect("connection");
                // SAFETY: both endpoints retained, opposite direction of the connection.
                check(unsafe { b.connect(a.as_ptr()) }, "controller connect")?;
            }
        }
        plugin.refresh_buses()?;
        // SAFETY: initialized processor queried on the main thread.
        let f64 = unsafe { plugin.shared.processor.canProcessSampleSize(1) } == kResultOk;
        let event_count = |direction| -> Result<u16, PluginError> {
            // SAFETY: initialized component, valid event media type and direction.
            let count = unsafe { plugin.component.getBusCount(1, direction) };
            if !(0..=32).contains(&count) {
                return Err(PluginError("invalid event bus count".into()));
            }
            Ok(count as u16)
        };
        let editor = if let Some(controller) = &plugin.controller {
            // SAFETY: initialized controller queried on its main thread; nullable owned result.
            let raw = unsafe { controller.createView(ViewType::kEditor) };
            // SAFETY: createView transfers one reference, immediately released after capability probe.
            unsafe { ComPtr::from_raw(raw) }.is_some()
        } else {
            false
        };
        plugin.capabilities = vst_audio_protocol::Capabilities {
            f64,
            input_event_buses: event_count(0)?,
            output_event_buses: event_count(1)?,
            editor,
            sysex: true,
            sample_accurate_parameters: true,
        };
        // SAFETY: initialized audio processor, main-thread latency query.
        plugin.info.latency = unsafe { plugin.shared.processor.getLatencySamples() };
        Ok(plugin)
    }
    fn buses(&self, direction: i32) -> Result<Vec<vst_audio_protocol::BusInfo>, PluginError> {
        // SAFETY: initialized component queried on its owning thread.
        let count = unsafe { self.component.getBusCount(0, direction) };
        if !(0..=32).contains(&count) {
            return Err(PluginError("invalid audio bus count".into()));
        }
        let mut buses = Vec::new();
        let mut total = 0;
        for index in 0..count {
            // SAFETY: SDK BusInfo has valid all-zero primitive representation.
            let mut info: BusInfo = unsafe { std::mem::zeroed() };
            check(
                // SAFETY: enumerated audio bus and writable information output.
                unsafe { self.component.getBusInfo(0, direction, index, &mut info) },
                "getBusInfo",
            )?;
            if !(0..=64).contains(&info.channelCount) {
                return Err(PluginError("invalid bus channels".into()));
            }
            total += info.channelCount;
            if total > 64 {
                return Err(PluginError("too many audio channels".into()));
            }
            let mut arrangement = 0;
            // SAFETY: valid bus index, live processor and writable arrangement output.
            let result = unsafe {
                self.shared
                    .processor
                    .getBusArrangement(direction, index, &mut arrangement)
            };
            if result != kResultOk {
                arrangement = 0;
            }
            if arrangement != 0 && arrangement.count_ones() != info.channelCount as u32 {
                return Err(PluginError(
                    "bus arrangement disagrees with channel count".into(),
                ));
            }
            let mut name = String::from_utf16_lossy(
                &info.name[..info
                    .name
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(info.name.len())],
            );
            if name.len() > 128 {
                let mut end = 128;
                while !name.is_char_boundary(end) {
                    end -= 1;
                }
                name.truncate(end);
            }
            buses.push(vst_audio_protocol::BusInfo {
                layout: vst_audio_protocol::Bus {
                    channels: info.channelCount as u16,
                    arrangement,
                },
                kind: if info.busType == BusTypes_::kMain as i32 {
                    vst_audio_protocol::BusKind::Main
                } else {
                    vst_audio_protocol::BusKind::Aux
                },
                name,
                default_active: (info.flags as i64) & (BusInfo_::BusFlags_::kDefaultActive as i64)
                    != 0,
            });
        }
        Ok(buses)
    }
    fn refresh_buses(&mut self) -> Result<(), PluginError> {
        self.info.input_buses = self.buses(0)?;
        self.info.output_buses = self.buses(1)?;
        self.info.inputs = self
            .info
            .input_buses
            .iter()
            .map(|b| b.layout.channels)
            .sum();
        self.info.outputs = self
            .info
            .output_buses
            .iter()
            .map(|b| b.layout.channels)
            .sum();
        self.info.input = self
            .info
            .input_buses
            .first()
            .map_or(0, |b| b.layout.arrangement);
        self.info.output = self
            .info
            .output_buses
            .first()
            .map_or(0, |b| b.layout.arrangement);
        Ok(())
    }
    pub fn capabilities(&self) -> vst_audio_protocol::Capabilities {
        self.capabilities
    }
    pub fn info(&self) -> Info {
        self.info.clone()
    }
    pub fn configure(
        &mut self,
        rate: u32,
        frames: u32,
        inputs: u16,
        outputs: u16,
    ) -> Result<(), PluginError> {
        if inputs != self.info.inputs || outputs != self.info.outputs {
            return Err(PluginError("configuration channel mismatch".into()));
        }
        self.configure_audio(&vst_audio_protocol::ProcessingSetup {
            sample_rate: rate,
            quantum: frames,
            sample_format: vst_audio_ring::SampleFormat::F32,
            offline: false,
            inputs: self
                .info
                .input_buses
                .iter()
                .map(|bus| bus.configuration(true))
                .collect(),
            outputs: self
                .info
                .output_buses
                .iter()
                .map(|bus| bus.configuration(true))
                .collect(),
        })
    }
    pub fn configure_audio(
        &mut self,
        configuration: &vst_audio_protocol::ProcessingSetup,
    ) -> Result<(), PluginError> {
        use vst_audio_ring::SampleFormat;
        if self.shared.active.load(Ordering::SeqCst)
            || self.processor_taken
            || !configuration.valid()
            || configuration.inputs.len() != self.info.input_buses.len()
            || configuration.outputs.len() != self.info.output_buses.len()
        {
            return Err(PluginError("invalid VST3 configuration".into()));
        }
        let mut input: Vec<_> = configuration
            .inputs
            .iter()
            .map(|b| b.layout.arrangement)
            .collect();
        let mut output: Vec<_> = configuration
            .outputs
            .iter()
            .map(|b| b.layout.arrangement)
            .collect();
        // SAFETY: inactive component; complete arrays cover declared buses. Read-back below validates even a rejection.
        let _ = unsafe {
            self.shared.processor.setBusArrangements(
                input.as_mut_ptr(),
                input.len() as i32,
                output.as_mut_ptr(),
                output.len() as i32,
            )
        };
        self.refresh_buses()?;
        for (requested, actual) in configuration
            .inputs
            .iter()
            .zip(&self.info.input_buses)
            .chain(configuration.outputs.iter().zip(&self.info.output_buses))
        {
            if requested.layout.channels != actual.layout.channels
                || (requested.layout.arrangement != 0
                    && requested.layout.arrangement != actual.layout.arrangement)
            {
                return Err(PluginError("plugin rejected bus arrangement".into()));
            }
        }
        let precision = if configuration.sample_format == SampleFormat::F64 {
            1
        } else {
            0
        };
        check(
            // SAFETY: main-thread query before setupProcessing.
            unsafe { self.shared.processor.canProcessSampleSize(precision) },
            "sample precision",
        )?;
        let mut setup = ProcessSetup {
            processMode: if configuration.offline { 2 } else { 0 },
            symbolicSampleSize: precision,
            maxSamplesPerBlock: configuration.quantum as i32,
            sampleRate: f64::from(configuration.sample_rate),
        };
        check(
            // SAFETY: fully initialized setup, inactive processor on main thread.
            unsafe { self.shared.processor.setupProcessing(&mut setup) },
            "setupProcessing",
        )?;
        for (direction, buses) in [(0, &configuration.inputs), (1, &configuration.outputs)] {
            for (index, bus) in buses.iter().enumerate() {
                check(
                    // SAFETY: enumerated audio bus, activation on the main thread.
                    unsafe {
                        self.component
                            .activateBus(0, direction, index as i32, u8::from(bus.active))
                    },
                    "activate audio bus",
                )?;
            }
        }
        for (direction, count) in [
            (0, self.capabilities.input_event_buses),
            (1, self.capabilities.output_event_buses),
        ] {
            for index in 0..count {
                check(
                    // SAFETY: enumerated event bus, activate on the main thread before processing.
                    unsafe {
                        self.component
                            .activateBus(1, direction, i32::from(index), 1)
                    },
                    "activate event bus",
                )?;
            }
        }
        // SAFETY: main-thread query after setupProcessing.
        self.info.latency = unsafe { self.shared.processor.getLatencySamples() };
        self.max_frames = configuration.quantum as usize;
        self.setup = Some(configuration.clone());
        Ok(())
    }
    pub fn activate(&mut self) -> Result<(), PluginError> {
        if self.max_frames == 0 {
            return Err(PluginError("configure before activate".into()));
        }
        // SAFETY: configured component, lifecycle calls stay on the main thread.
        check(unsafe { self.component.setActive(1) }, "setActive")?;
        self.shared.active.store(true, Ordering::SeqCst);
        Ok(())
    }
    pub fn deactivate(&mut self) {
        let _control = self.shared.control();
        if self.shared.active.swap(false, Ordering::SeqCst) {
            // SAFETY: host joined audio and called setProcessing(false) before deactivation.
            let _ = unsafe { self.component.setActive(0) };
        }
    }
    pub fn take_processor(&mut self) -> Result<Vst3Processor, PluginError> {
        if self.processor_taken || self.max_frames == 0 {
            return Err(PluginError(
                "VST 3 processor already taken or not configured".into(),
            ));
        }
        self.processor_taken = true;
        let setup = self.setup.as_ref().expect("configured").clone();
        let changes = Changes::new();
        let pointer = changes.to_com_ptr().expect("parameter changes");
        let output_changes = Changes::new();
        let output_pointer = output_changes
            .to_com_ptr()
            .expect("output parameter changes");
        let input_events = crate::vst3_events::EventList::new();
        let input_events_ptr = input_events.to_com_ptr().expect("input event list");
        let output_events = crate::vst3_events::EventList::new();
        let output_events_ptr = output_events.to_com_ptr().expect("output event list");
        let mut mappings = Vec::new();
        if let Some(mapping) = self
            .controller
            .as_ref()
            .and_then(|c| c.cast::<IMidiMapping>())
        {
            for bus in 0..self.capabilities.input_event_buses {
                for channel in 0..16 {
                    for cc in 0..130 {
                        let mut id = 0;
                        // SAFETY: UI-thread controller mapping query with valid MIDI ranges and writable output.
                        if unsafe {
                            mapping.getMidiControllerAssignment(
                                i32::from(bus),
                                channel,
                                cc,
                                &mut id,
                            )
                        } == kResultOk
                        {
                            mappings.push((
                                (u32::from(bus) << 16) | ((channel as u32) << 8) | cc as u32,
                                id,
                                0,
                            ));
                        }
                    }
                }
            }
        }
        // Program changes are parameters associated with units, not CC number
        // 130 (that number is reserved for legacy MIDI *output* events).
        if let Some(controller) = &self.controller {
            let units = controller.cast::<IUnitInfo>();
            // SAFETY: initialized controller queried on its main thread.
            let count = unsafe { controller.getParameterCount() }.clamp(0, 65536);
            let mut programs = Vec::new();
            for index in 0..count {
                // SAFETY: ParameterInfo has primitive fields with valid zero representations.
                let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
                // SAFETY: writable parameter record and in-range parameter index.
                if unsafe { controller.getParameterInfo(index, &mut info) } == kResultOk
                    && info.flags & (1 << 15) != 0
                    && info.stepCount > 0
                {
                    programs.push((info.unitId, info.id, info.stepCount as u32));
                }
            }
            for bus in 0..self.capabilities.input_event_buses {
                for channel in 0..16 {
                    let mut unit = 0;
                    if let Some(units) = &units {
                        // SAFETY: live unit interface, valid event bus/channel, writable unit id.
                        let _ =
                            unsafe { units.getUnitByBus(1, 0, i32::from(bus), channel, &mut unit) };
                    }
                    if let Some((_, id, steps)) = programs.iter().find(|p| p.0 == unit) {
                        mappings.push((
                            (u32::from(bus) << 16) | ((channel as u32) << 8) | 130,
                            *id,
                            *steps,
                        ));
                    }
                }
            }
        }
        mappings.sort_unstable_by_key(|p| p.0);
        let mut parameter_ids = Vec::new();
        let mut start = 0;
        loop {
            let (total, page) = self.parameters(start, 32)?;
            parameter_ids.extend(page.into_iter().filter(|p| !p.read_only).map(|p| p.id));
            start += 32;
            if start >= total {
                break;
            }
        }
        parameter_ids.sort_unstable();
        parameter_ids.dedup();
        Ok(Vst3Processor {
            shared: self.shared.clone(),
            changes,
            pointer,
            output_changes,
            output_pointer,
            input_events,
            input_events_ptr,
            output_events,
            output_events_ptr,
            mappings,
            parameter_ids,
            inputs: vst_audio_protocol::ProcessingSetup::channels(&setup.inputs) as usize,
            outputs: vst_audio_protocol::ProcessingSetup::channels(&setup.outputs) as usize,
            frames: self.max_frames,
            processing: false,
            setup,
            capabilities: self.capabilities,
        })
    }
    pub fn parameters(
        &mut self,
        start: u32,
        count: u16,
    ) -> Result<(u32, Vec<vst_audio_protocol::Parameter>), PluginError> {
        let Some(controller) = &self.controller else {
            return Ok((0, Vec::new()));
        };
        // SAFETY: live initialized controller on its creating main thread.
        let total = unsafe { controller.getParameterCount() };
        if !(0..=16384).contains(&total) || start > total as u32 || count == 0 || count > 32 {
            return Err(PluginError("invalid parameter page".into()));
        }
        let mut entries = Vec::new();
        for index in start..start.saturating_add(u32::from(count)).min(total as u32) {
            // SAFETY: ParameterInfo is a plain fixed-array/integer/float SDK record.
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            check(
                // SAFETY: queried parameter index and writable SDK output.
                unsafe { controller.getParameterInfo(index as i32, &mut info) },
                "getParameterInfo",
            )?;
            // SAFETY: ID returned by this controller's parameter enumeration.
            let value = unsafe { controller.getParamNormalized(info.id) };
            if !value.is_finite()
                || !(0.0..=1.0).contains(&value)
                || !info.defaultNormalizedValue.is_finite()
                || !(0.0..=1.0).contains(&info.defaultNormalizedValue)
                || info.stepCount < 0
            {
                return Err(PluginError("invalid parameter metadata".into()));
            }
            let wide = |s: &[u16]| {
                String::from_utf16_lossy(&s[..s.iter().position(|c| *c == 0).unwrap_or(s.len())])
            };
            entries.push(vst_audio_protocol::Parameter {
                id: info.id,
                name: wide(&info.title),
                unit: wide(&info.units),
                value,
                default: Some(info.defaultNormalizedValue),
                steps: Some(info.stepCount as u32),
                automatable: info.flags & 1 != 0,
                read_only: info.flags & 2 != 0,
                hidden: info.flags & (1 << 4) != 0,
            });
        }
        Ok((total as u32, entries))
    }
    pub fn set_parameter(&mut self, id: u32, value: f64) -> Result<(), PluginError> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(PluginError("invalid parameter value".into()));
        }
        let (total, first) = self.parameters(0, 32)?;
        let mut found = first.iter().any(|p| p.id == id && !p.read_only);
        let mut start = 32;
        while !found && start < total {
            found = self
                .parameters(start, 32)?
                .1
                .iter()
                .any(|p| p.id == id && !p.read_only);
            start += 32;
        }
        if !found {
            return Err(PluginError("unknown or read-only parameter".into()));
        }
        let controller = self
            .controller
            .as_ref()
            .ok_or_else(|| PluginError("no controller".into()))?;
        check(
            // SAFETY: main-thread live controller, validated ID and normalized value.
            unsafe { controller.setParamNormalized(id, value) },
            "setParamNormalized",
        )?;
        if !self.shared.edits.push(id, value) {
            return Err(PluginError("parameter queue full".into()));
        }
        Ok(())
    }
    pub fn get_state(&mut self) -> Result<(Vec<u8>, Vec<u8>), PluginError> {
        // IComponent documents getState in Processing state: do NOT close admission.
        let component = Stream::new(Vec::new());
        let pointer = component
            .to_com_ptr::<IBStream>()
            .expect("stream interface");
        check(
            // SAFETY: main-thread state call, owned writable stream; SDK permits concurrent process.
            unsafe { self.component.getState(pointer.as_ptr()) },
            "component getState",
        )?;
        let controller = Stream::new(Vec::new());
        if let Some(editor) = &self.controller {
            let pointer = controller
                .to_com_ptr::<IBStream>()
                .expect("stream interface");
            // SAFETY: main-thread controller state call with owned stream.
            let result = unsafe { editor.getState(pointer.as_ptr()) };
            if result != kNotImplemented {
                check(result, "controller getState")?;
            }
        }
        let snapshot = |stream: &Stream| -> Result<Vec<u8>, PluginError> {
            stream
                .0
                .try_lock()
                .map_err(|_| PluginError("plugin state stream is still in use".into()))?
                .try_snapshot()
                .map_err(|_| PluginError("out of memory for plugin state".into()))
        };
        Ok((snapshot(&component)?, snapshot(&controller)?))
    }
    pub fn set_state(&mut self, component: &[u8], controller: &[u8]) -> Result<(), PluginError> {
        let _control = self.shared.control();
        let stream = Stream::try_from_slice(component)
            .map_err(|_| PluginError("out of memory for plugin state".into()))?;
        let pointer = stream.to_com_ptr::<IBStream>().expect("stream interface");
        check(
            // SAFETY: main thread, admitted processing excluded, stream owns component bytes.
            unsafe { self.component.setState(pointer.as_ptr()) },
            "component setState",
        )?;
        if let Some(editor) = &self.controller {
            let stream = Stream::try_from_slice(component)
                .map_err(|_| PluginError("out of memory for plugin state".into()))?;
            let pointer = stream.to_com_ptr::<IBStream>().expect("stream interface");
            // SAFETY: main thread; fresh stream rewound to start of component state.
            let result = unsafe { editor.setComponentState(pointer.as_ptr()) };
            if result != kNotImplemented {
                check(result, "setComponentState")?;
            }
            let stream = Stream::try_from_slice(controller)
                .map_err(|_| PluginError("out of memory for plugin state".into()))?;
            let pointer = stream.to_com_ptr::<IBStream>().expect("stream interface");
            // SAFETY: controller UI state restored after the component state, on main thread.
            let result = unsafe { editor.setState(pointer.as_ptr()) };
            if result != kNotImplemented {
                check(result, "controller setState")?;
            }
        }
        Ok(())
    }
    pub fn open_editor(&mut self) -> Result<(), PluginError> {
        self.open_editor_at(None)
    }
    pub fn open_editor_in(
        &mut self,
        parent: vst_audio_protocol::ParentWindow,
    ) -> Result<(), PluginError> {
        self.open_editor_at(Some(parent))
    }
    fn open_editor_at(
        &mut self,
        parent: Option<vst_audio_protocol::ParentWindow>,
    ) -> Result<(), PluginError> {
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.window.parent() == parent)
        {
            return Ok(());
        }
        self.editor = None;
        let controller = self
            .controller
            .as_ref()
            .ok_or_else(|| PluginError("no VST 3 controller".into()))?;
        // SAFETY: controller queried on main thread with SDK editor view name.
        let pointer = unsafe { controller.createView(ViewType::kEditor) };
        // SAFETY: createView transfers an owned interface reference, nullable for no editor.
        let view = unsafe { ComPtr::from_raw(pointer) }
            .ok_or_else(|| PluginError("no VST 3 editor".into()))?;
        check(
            // SAFETY: live view, SDK Windows platform string, on main thread.
            unsafe { view.isPlatformTypeSupported(crate::editor_platform().as_ptr()) },
            "native editor support",
        )?;
        let mut size = ViewRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: writable view rectangle, queried before attachment.
        check(unsafe { view.getSize(&mut size) }, "getSize")?;
        let width = size
            .right
            .checked_sub(size.left)
            .filter(|v| (1..=8192).contains(v))
            .ok_or_else(|| PluginError("invalid editor width".into()))?;
        let height = size
            .bottom
            .checked_sub(size.top)
            .filter(|v| (1..=8192).contains(v))
            .ok_or_else(|| PluginError("invalid editor height".into()))?;
        let window = Rc::new(match parent {
            Some(parent) => EditorWindow::open_embedded(parent, width, height)
                .map_err(|e| PluginError(e.to_string()))?,
            None => EditorWindow::open(&self.info.class.name, width, height)
                .map_err(|e| PluginError(e.to_string()))?,
        });
        let frame = ComWrapper::new(Frame {
            window: window.clone(),
            #[cfg(target_os = "linux")]
            run_loop: self.module.run_loop.clone(),
        })
        .to_com_ptr::<IPlugFrame>()
        .expect("frame interface");
        // SAFETY: retained frame and main-thread view; frame owns the window lifetime.
        check(unsafe { view.setFrame(frame.as_ptr()) }, "setFrame")?;
        // SAFETY: live host-owned HWND, platform type matches support query.
        let result = unsafe {
            view.attached(
                window.handle_value() as *mut c_void,
                crate::editor_platform().as_ptr(),
            )
        };
        if result != kResultOk {
            // SAFETY: release installed frame before returning attachment error.
            let _ = unsafe { view.setFrame(std::ptr::null_mut()) };
            check(result, "attached")?;
        }
        self.editor = Some(Editor {
            scale: 1.0,
            view,
            _frame: frame,
            window,
        });
        Ok(())
    }
    pub fn editor_action(
        &mut self,
        action: vst_audio_protocol::EditorAction,
    ) -> Result<vst_audio_protocol::EditorReply, PluginError> {
        use vst_audio_protocol::{EditorAction as A, EditorReply, EditorStatus};
        let editor = self
            .editor
            .as_mut()
            .ok_or_else(|| PluginError("editor is not open".into()))?;
        let scale_interface = editor.view.cast::<IPlugViewContentScaleSupport>();
        // SAFETY: queried on the creating main thread with a retained live view.
        let resize_supported = unsafe { editor.view.canResize() } == kResultTrue;
        match action {
            A::Show => editor
                .window
                .show(true)
                .map_err(|e| PluginError(e.to_string()))?,
            A::Hide => editor
                .window
                .show(false)
                .map_err(|e| PluginError(e.to_string()))?,
            A::Focus => editor
                .window
                .focus()
                .map_err(|e| PluginError(e.to_string()))?,
            A::Inspect => {}
            A::Resize { width, height } => {
                if !resize_supported || width == 0 || height == 0 || width > 8192 || height > 8192 {
                    return Err(PluginError("editor resize unsupported or invalid".into()));
                }
                let mut rect = ViewRect {
                    left: 0,
                    top: 0,
                    right: width as i32,
                    bottom: height as i32,
                };
                // SAFETY: live main-thread view and writable size constraint rectangle.
                check(
                    // SAFETY: live main-thread view and writable rectangle.
                    unsafe { editor.view.checkSizeConstraint(&mut rect) },
                    "checkSizeConstraint",
                )?;
                let width = rect
                    .right
                    .checked_sub(rect.left)
                    .filter(|v| (1..=8192).contains(v))
                    .ok_or_else(|| PluginError("invalid constrained width".into()))?;
                let height = rect
                    .bottom
                    .checked_sub(rect.top)
                    .filter(|v| (1..=8192).contains(v))
                    .ok_or_else(|| PluginError("invalid constrained height".into()))?;
                // SAFETY: live view with validated writable rectangle on main thread.
                check(unsafe { editor.view.onSize(&mut rect) }, "onSize")?;
                editor
                    .window
                    .resize(width, height)
                    .map_err(|e| PluginError(e.to_string()))?;
            }
            A::Scale(scale) => {
                if !scale.is_finite() || !(0.5..=4.0).contains(&scale) {
                    return Err(PluginError("invalid content scale".into()));
                }
                let interface = scale_interface
                    .as_ref()
                    .ok_or_else(|| PluginError("editor scaling unsupported".into()))?;
                // SAFETY: retained optional view interface; bounded finite scale on main thread.
                check(
                    // SAFETY: retained interface and validated finite scale.
                    unsafe { interface.setContentScaleFactor(scale) },
                    "setContentScaleFactor",
                )?;
                editor.scale = scale;
            }
            A::Capture => {
                return editor
                    .window
                    .capture()
                    .map(EditorReply::Image)
                    .map_err(|e| PluginError(e.to_string()));
            }
        }
        let (visible, width, height) = editor
            .window
            .status()
            .map_err(|e| PluginError(e.to_string()))?;
        Ok(EditorReply::Status(EditorStatus {
            visible,
            embedded: editor.window.parent().is_some(),
            width,
            height,
            scale: editor.scale,
            resize_supported,
            scale_supported: scale_interface.is_some(),
        }))
    }
    pub fn request_editor_close(&self) -> Result<(), PluginError> {
        if let Some(editor) = &self.editor {
            editor
                .window
                .request_close()
                .map_err(|e| PluginError(e.to_string()))?;
        }
        Ok(())
    }
    pub fn idle(&mut self) {
        #[cfg(target_os = "linux")]
        self.module.run_loop.poll();
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.window.pump())
        {
            self.editor = None;
            self.closed_editor = true;
        }
    }
    pub fn take_events(&mut self) -> Events {
        let flags = self.shared.edits.flags.swap(0, Ordering::AcqRel);
        let latency = if flags & 2 != 0 {
            // SAFETY: SDK latency notification handled on main thread, live processor.
            self.info.latency = unsafe { self.shared.processor.getLatencySamples() };
            Some(self.info.latency)
        } else {
            None
        };
        Events {
            params: flags & 1 != 0,
            latency,
            closed: std::mem::take(&mut self.closed_editor),
            fault: if flags & 4 != 0 {
                Some("bus layout changed")
            } else if flags & 8 != 0 {
                Some("parameter queue overflow")
            } else if flags & 16 != 0 {
                Some("VST 3 audio processing failed")
            } else {
                None
            },
        }
    }
}
impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        self.editor = None;
        self.deactivate();
        let _control = self.shared.control();
        self.shared.closed.store(true, Ordering::SeqCst);
        if let Some((a, b)) = self.connection.take() {
            // SAFETY: main-thread connected endpoints still retained through disconnect.
            let _ = unsafe { a.disconnect(b.as_ptr()) };
            // SAFETY: disconnect reverse endpoint before termination.
            let _ = unsafe { b.disconnect(a.as_ptr()) };
        }
        if let Some(controller) = &self.controller {
            // SAFETY: release handler before controller termination, on creating thread.
            let _ = unsafe { controller.setComponentHandler(std::ptr::null_mut()) };
            if self.controller_initialized {
                // SAFETY: separately initialized controller terminated once on main thread.
                let _ = unsafe { controller.terminate() };
            }
        }
        if self.component_initialized {
            // SAFETY: component initialized once; audio joined and controller disconnected.
            let _ = unsafe { self.component.terminate() };
        }
    }
}
pub struct Vst3Processor {
    shared: Arc<Shared>,
    changes: ComWrapper<Changes>,
    pointer: ComPtr<IParameterChanges>,
    output_changes: ComWrapper<Changes>,
    output_pointer: ComPtr<IParameterChanges>,
    input_events: ComWrapper<crate::vst3_events::EventList>,
    input_events_ptr: ComPtr<IEventList>,
    output_events: ComWrapper<crate::vst3_events::EventList>,
    output_events_ptr: ComPtr<IEventList>,
    mappings: Vec<(u32, u32, u32)>,
    parameter_ids: Vec<u32>,
    inputs: usize,
    outputs: usize,
    frames: usize,
    processing: bool,
    setup: vst_audio_protocol::ProcessingSetup,
    capabilities: vst_audio_protocol::Capabilities,
}
trait Sample: Copy {
    const FORMAT: vst_audio_ring::SampleFormat;
    fn pointers(pointer: *mut *mut Self) -> AudioBusBuffers__type0;
}
impl Sample for f32 {
    const FORMAT: vst_audio_ring::SampleFormat = vst_audio_ring::SampleFormat::F32;
    fn pointers(pointer: *mut *mut Self) -> AudioBusBuffers__type0 {
        AudioBusBuffers__type0 {
            channelBuffers32: pointer,
        }
    }
}
impl Sample for f64 {
    const FORMAT: vst_audio_ring::SampleFormat = vst_audio_ring::SampleFormat::F64;
    fn pointers(pointer: *mut *mut Self) -> AudioBusBuffers__type0 {
        AudioBusBuffers__type0 {
            channelBuffers64: pointer,
        }
    }
}
impl std::fmt::Debug for Vst3Processor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Processor").finish_non_exhaustive()
    }
}
// SAFETY: Changes contains Cells but is owned by this unique audio handle; no
// other thread receives its interfaces. All buffers and COM calls stay on audio.
unsafe impl Send for Vst3Processor {}
impl Vst3Processor {
    pub fn process(&mut self, planes: &mut [&mut [f32]], frames: usize) -> bool {
        self.process_block(
            planes,
            frames,
            &vst_audio_ring::Transport::default(),
            &[],
            &mut vst_audio_ring::EventBuffer::default(),
        )
    }
    pub fn process_block(
        &mut self,
        planes: &mut [&mut [f32]],
        frames: usize,
        context: &vst_audio_ring::Transport,
        input: &[vst_audio_ring::Event],
        output: &mut vst_audio_ring::EventBuffer,
    ) -> bool {
        self.process_inner(planes, frames, context, input, output)
    }
    pub fn process_block_f64(
        &mut self,
        planes: &mut [&mut [f64]],
        frames: usize,
        context: &vst_audio_ring::Transport,
        input: &[vst_audio_ring::Event],
        output: &mut vst_audio_ring::EventBuffer,
    ) -> bool {
        self.process_inner(planes, frames, context, input, output)
    }
    fn process_inner<T: Sample>(
        &mut self,
        planes: &mut [&mut [T]],
        frames: usize,
        context: &vst_audio_ring::Transport,
        input: &[vst_audio_ring::Event],
        output: &mut vst_audio_ring::EventBuffer,
    ) -> bool {
        use vst_audio_ring::{EventData, MAX_EVENTS};
        output.clear();
        if T::FORMAT != self.setup.sample_format
            || frames == 0
            || frames > self.frames
            || planes.len() < self.inputs.max(self.outputs)
            || planes.iter().any(|p| p.len() < frames)
            || !context.valid()
            || input.len() > MAX_EVENTS
            || input.iter().any(|e| !e.valid(frames as u32))
            || input
                .windows(2)
                .any(|p| p[0].sample_offset > p[1].sample_offset)
            || self.shared.closed.load(Ordering::SeqCst)
            || !self.shared.admitted.load(Ordering::SeqCst)
            || self.shared.busy.swap(true, Ordering::SeqCst)
        {
            return false;
        }
        let result = (|| {
            if !self.shared.admitted.load(Ordering::SeqCst)
                || !self.shared.active.load(Ordering::SeqCst)
                || self.shared.closed.load(Ordering::SeqCst)
            {
                return false;
            }
            if !self.processing {
                // SAFETY: sole audio handle, active configured component, entering processing.
                if unsafe { self.shared.processor.setProcessing(1) } != kResultOk {
                    return false;
                }
                self.processing = true;
            }
            let mut start = 0;
            let mut base_context = *context;
            let mut context_origin = 0;
            while start < frames {
                for event in input.iter().filter(|e| e.sample_offset as usize == start) {
                    if let EventData::Transport(value) = event.data {
                        base_context = value;
                        context_origin = start;
                    }
                }
                let end = input
                    .iter()
                    .filter(|e| {
                        e.sample_offset as usize > start
                            && matches!(e.data, EventData::Transport(_))
                    })
                    .map(|e| e.sample_offset as usize)
                    .next()
                    .unwrap_or(frames);
                let context = base_context.advanced(start - context_origin, self.setup.sample_rate);
                if start == 0 {
                    self.changes.drain(&self.shared.edits);
                } else {
                    self.changes.clear();
                }
                self.output_changes.clear();
                self.input_events.reset(
                    frames as u32,
                    start as u32,
                    self.capabilities.input_event_buses,
                );
                self.output_events.reset(
                    frames as u32,
                    start as u32,
                    self.capabilities.output_event_buses,
                );
                self.input_events.fill(
                    input,
                    start,
                    end,
                    context,
                    self.setup.sample_rate,
                    &self.changes,
                    &self.mappings,
                    &self.parameter_ids,
                    &mut output.dropped,
                );
                let mut input_pointers = [std::ptr::null_mut(); 64];
                let mut output_pointers = [std::ptr::null_mut(); 64];
                for (index, plane) in planes.iter_mut().enumerate().take(64) {
                    if index < self.inputs {
                        input_pointers[index] = plane.as_mut_ptr().wrapping_add(start);
                    }
                    if index < self.outputs {
                        output_pointers[index] = plane.as_mut_ptr().wrapping_add(start);
                    }
                }
                let empty = AudioBusBuffers {
                    numChannels: 0,
                    silenceFlags: 0,
                    __field0: T::pointers(std::ptr::null_mut()),
                };
                let mut inputs = [empty; 32];
                let mut outputs = [empty; 32];
                for (buses, pointers, target) in [
                    (&self.setup.inputs, &mut input_pointers, &mut inputs),
                    (&self.setup.outputs, &mut output_pointers, &mut outputs),
                ] {
                    let mut offset = 0;
                    for (index, bus) in buses.iter().enumerate() {
                        let channels = if bus.active {
                            usize::from(bus.layout.channels)
                        } else {
                            0
                        };
                        target[index] = AudioBusBuffers {
                            numChannels: channels as i32,
                            silenceFlags: 0,
                            __field0: T::pointers(if channels == 0 {
                                std::ptr::null_mut()
                            } else {
                                pointers.as_mut_ptr().wrapping_add(offset)
                            }),
                        };
                        offset += channels;
                    }
                }
                let mut native_context =
                    crate::vst3_events::process_context(context, self.setup.sample_rate);
                let mut data = ProcessData {
                    processMode: if self.setup.offline { 2 } else { 0 },
                    symbolicSampleSize: self.setup.sample_format as i32,
                    numSamples: (end - start) as i32,
                    numInputs: self.setup.inputs.len() as i32,
                    numOutputs: self.setup.outputs.len() as i32,
                    inputs: if self.setup.inputs.is_empty() {
                        std::ptr::null_mut()
                    } else {
                        inputs.as_mut_ptr()
                    },
                    outputs: if self.setup.outputs.is_empty() {
                        std::ptr::null_mut()
                    } else {
                        outputs.as_mut_ptr()
                    },
                    inputParameterChanges: self.pointer.as_ptr(),
                    outputParameterChanges: self.output_pointer.as_ptr(),
                    inputEvents: self.input_events_ptr.as_ptr(),
                    outputEvents: self.output_events_ptr.as_ptr(),
                    processContext: &mut native_context,
                };
                // SAFETY: exclusive admitted audio call, complete native bus arrays and bounded live buffers/event interfaces.
                if unsafe { self.shared.processor.process(&mut data) } != kResultOk {
                    return false;
                }
                self.output_events.collect(output);
                self.output_changes
                    .collect(start as u32, frames as u32, output);
                start = end;
            }
            true
        })();
        if !result {
            self.shared.edits.flags.fetch_or(16, Ordering::Release);
        }
        self.shared.busy.store(false, Ordering::SeqCst);
        result
    }
    pub fn stop(&mut self) {
        if !self.shared.admitted.load(Ordering::SeqCst)
            || self.shared.busy.swap(true, Ordering::SeqCst)
        {
            return;
        }
        if self.processing
            && self.shared.admitted.load(Ordering::SeqCst)
            && !self.shared.closed.load(Ordering::SeqCst)
        {
            // SAFETY: sole audio thread holds admission after its last process, before teardown.
            let _ = unsafe { self.shared.processor.setProcessing(0) };
            self.processing = false;
        }
        self.shared.busy.store(false, Ordering::SeqCst);
    }
}

#[cfg(target_os = "linux")]
impl Drop for Vst3Module {
    fn drop(&mut self) {
        self.run_loop.clear();
    }
}
