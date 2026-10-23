#![allow(unsafe_code, non_snake_case)]
//! Guest backend on the host's display, with its own private event queue.
use std::{
    cell::RefCell,
    ffi::c_void,
    fs::File,
    io::{self, Write},
    os::fd::{AsFd, AsRawFd, FromRawFd},
    rc::Rc,
};
use vst3::{
    Class, ComPtr, ComWrapper,
    Steinberg::{Linux::*, Vst::*, *},
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop,
    protocol::{
        wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_subcompositor,
        wl_subsurface, wl_surface,
    },
};
fn error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
#[derive(Default)]
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    sub: Option<wl_subcompositor::WlSubcompositor>,
    shm: Option<wl_shm::WlShm>,
    buffers: Vec<wl_buffer::WlBuffer>,
}
struct Guest {
    queue: EventQueue<State>,
    state: State,
}
struct Handler {
    connection: Connection,
    guest: Rc<RefCell<Guest>>,
}
impl Class for Handler {
    type Interfaces = (IEventHandler,);
}
impl IEventHandlerTrait for Handler {
    unsafe fn onFDIsSet(&self, _: i32) {
        let backend = self.connection.backend();
        if let Some(read) = backend.prepare_read() {
            let _ = read.read();
        }
        let _ = backend.dispatch_inner_queue();
        let mut guest = self.guest.borrow_mut();
        let Guest { queue, state } = &mut *guest;
        let _ = queue.dispatch_pending(state);
        let _ = self.connection.flush();
    }
}
pub(crate) struct Canvas {
    guest: Rc<RefCell<Guest>>,
    connection: Connection,
    surface: wl_surface::WlSurface,
    sub: wl_subsurface::WlSubsurface,
    handler: ComPtr<IEventHandler>,
    run_loop: ComPtr<IRunLoop>,
    host: ComPtr<IWaylandHost>,
}
impl Canvas {
    pub unsafe fn attach(
        context: &ComPtr<IHostApplication>,
        parent: *mut c_void,
    ) -> io::Result<Self> {
        let mut pointer = std::ptr::null_mut();
        let mut iid = IWaylandHost_iid;
        let mut cid = IWaylandHost_iid;
        // SAFETY: live host context and writable interface identifiers/output.
        if unsafe { context.createInstance(&mut cid, &mut iid, &mut pointer) } != kResultOk {
            return Err(error("Wayland host unavailable"));
        }
        // SAFETY: successful createInstance transfers one IWaylandHost reference.
        let host = unsafe { ComPtr::<IWaylandHost>::from_raw(pointer.cast()) }
            .ok_or_else(|| error("null host"))?;
        let queried = context
            .cast::<IWaylandHost>()
            .ok_or_else(|| error("queryInterface missing Wayland host"))?;
        if queried.as_ptr() != host.as_ptr() {
            return Err(error("createInstance and queryInterface disagree"));
        }
        // SAFETY: UI-thread call on retained host interface.
        let display = unsafe { host.openWaylandConnection() };
        if display.is_null() {
            return Err(error("null display"));
        }
        // SAFETY: host retains its connection until after removed and close; this guest never disconnects it.
        let backend =
            unsafe { wayland_backend::client::Backend::from_foreign_display(display.cast()) };
        let connection = Connection::from_backend(backend);
        let result = Self::create(connection, host.clone(), context, parent);
        if result.is_err() {
            // SAFETY: matches the successful open above; failed guest objects have already dropped.
            unsafe { host.closeWaylandConnection(display) };
        }
        result
    }
    fn create(
        connection: Connection,
        host: ComPtr<IWaylandHost>,
        context: &ComPtr<IHostApplication>,
        parent: *mut c_void,
    ) -> io::Result<Self> {
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        connection.display().get_registry(&qh, ());
        let mut state = State::default();
        queue.roundtrip(&mut state).map_err(error)?;
        // SAFETY: attached supplies a live wl_surface on this exact host connection; borrowed only for construction.
        let id = unsafe {
            wayland_backend::client::ObjectId::from_ptr(
                wl_surface::WlSurface::interface(),
                parent.cast(),
            )
        }
        .map_err(error)?;
        let parent = wl_surface::WlSurface::from_id(&connection, id).map_err(error)?;
        let surface = state
            .compositor
            .as_ref()
            .ok_or_else(|| error("no compositor"))?
            .create_surface(&qh, ());
        let sub = state
            .sub
            .as_ref()
            .ok_or_else(|| error("no subcompositor"))?
            .get_subsurface(&surface, &parent, &qh, ());
        sub.set_desync();
        let run_loop = context
            .cast::<IRunLoop>()
            .ok_or_else(|| error("no run loop"))?;
        let guest = Rc::new(RefCell::new(Guest { queue, state }));
        let handler = ComWrapper::new(Handler {
            connection: connection.clone(),
            guest: guest.clone(),
        })
        .to_com_ptr::<IEventHandler>()
        .expect("event handler");
        // SAFETY: handler retained through unregister; connection keeps its descriptor live.
        let registered = unsafe {
            run_loop
                .registerEventHandler(handler.as_ptr(), connection.backend().poll_fd().as_raw_fd())
        };
        if registered != kResultOk {
            return Err(error("cannot register Wayland fd"));
        }
        let canvas = Self {
            guest,
            connection,
            surface,
            sub,
            handler,
            run_loop,
            host,
        };
        canvas.resize(320, 200)?;
        canvas.connection.flush().map_err(error)?;
        Ok(canvas)
    }
    pub fn resize(&self, width: i32, height: i32) -> io::Result<()> {
        let size = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(4))
            .filter(|n| *n > 0 && *n <= 64 * 1024 * 1024)
            .ok_or_else(|| error("invalid buffer"))?;
        // SAFETY: static terminated name; creates a new owned anonymous file.
        let fd = unsafe { libc::memfd_create(c"vst-audio-fixture".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: new owned fd from memfd_create, transferred exactly once.
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.set_len(size as u64)?;
        let row: Vec<u8> = (0..width).flat_map(|_| [128, 64, 32, 255]).collect();
        for _ in 0..height {
            file.write_all(&row)?;
        }
        let mut guest = self.guest.borrow_mut();
        let qh = guest.queue.handle();
        let pool = guest
            .state
            .shm
            .as_ref()
            .ok_or_else(|| error("no shm"))?
            .create_pool(file.as_fd(), size, &qh, ());
        let buffer = pool.create_buffer(
            0,
            width,
            height,
            width * 4,
            wl_shm::Format::Xrgb8888,
            &qh,
            (),
        );
        pool.destroy();
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface.damage(0, 0, width, height);
        self.surface.commit();
        guest.state.buffers.push(buffer);
        self.connection.flush().map_err(error)
    }
}
impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: live retained runloop and registered handler on the UI thread.
        unsafe { self.run_loop.unregisterEventHandler(self.handler.as_ptr()) };
        self.sub.destroy();
        self.surface.destroy();
        for buffer in self.guest.borrow_mut().state.buffers.drain(..) {
            buffer.destroy();
        }
        let _ = self.connection.flush();
        // SAFETY: balances open, all fixture protocol objects are destroyed; host owns the connection.
        unsafe {
            self.host
                .closeWaylandConnection(self.connection.backend().display_ptr().cast())
        };
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        s: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => s.compositor = Some(registry.bind(name, 4, qh, ())),
                "wl_subcompositor" => s.sub = Some(registry.bind(name, 1, qh, ())),
                "wl_shm" => s.shm = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
        }
    }
}
impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        s: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
            s.buffers.retain(|b| b != buffer);
        }
    }
}
delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(State: ignore wl_subsurface::WlSubsurface);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
