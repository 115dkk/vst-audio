//! Worker-owned X11 top-level window. No application GUI framework is involved.
use std::{cell::Cell, io};
use x11rb::{
    COPY_DEPTH_FROM_PARENT,
    connection::Connection,
    protocol::{Event, xproto::*},
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

#[derive(Debug)]
pub struct EditorWindow {
    connection: RustConnection,
    window: u32,
    delete: u32,
    protocols: u32,
    closed: Cell<bool>,
}
fn error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
impl EditorWindow {
    pub fn open(title: &str, width: i32, height: i32) -> io::Result<Self> {
        let (connection, screen) = x11rb::connect(None).map_err(error)?;
        let screen = &connection.setup().roots[screen];
        let window = connection.generate_id().map_err(error)?;
        connection
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                screen.root,
                0,
                0,
                width.clamp(1, 8192) as u16,
                height.clamp(1, 8192) as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .background_pixel(screen.white_pixel)
                    .event_mask(EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY),
            )
            .map_err(error)?
            .check()
            .map_err(error)?;
        let atom = |name| -> io::Result<u32> {
            Ok(connection
                .intern_atom(false, name)
                .map_err(error)?
                .reply()
                .map_err(error)?
                .atom)
        };
        let protocols = atom(b"WM_PROTOCOLS")?;
        let delete = atom(b"WM_DELETE_WINDOW")?;
        connection
            .change_property32(
                PropMode::REPLACE,
                window,
                protocols,
                AtomEnum::ATOM,
                &[delete],
            )
            .map_err(error)?;
        connection
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            )
            .map_err(error)?;
        connection.map_window(window).map_err(error)?;
        connection.flush().map_err(error)?;
        Ok(Self {
            connection,
            window,
            protocols,
            delete,
            closed: Cell::new(false),
        })
    }
    pub fn handle_value(&self) -> usize {
        self.window as usize
    }
    pub fn pump(&self) -> bool {
        while let Ok(Some(event)) = self.connection.poll_for_event() {
            match event {
                Event::ClientMessage(event)
                    if event.type_ == self.protocols
                        && event.data.as_data32()[0] == self.delete =>
                {
                    self.closed.set(true)
                }
                Event::DestroyNotify(_) => self.closed.set(true),
                _ => {}
            }
        }
        self.closed.get()
    }
    pub fn request_close(&self) -> io::Result<()> {
        self.closed.set(true);
        Ok(())
    }
    pub fn resize(&self, width: i32, height: i32) -> io::Result<()> {
        self.connection
            .configure_window(
                self.window,
                &ConfigureWindowAux::new()
                    .width(width.clamp(1, 8192) as u32)
                    .height(height.clamp(1, 8192) as u32),
            )
            .map_err(error)?;
        self.connection.flush().map_err(error)
    }
}
impl Drop for EditorWindow {
    fn drop(&mut self) {
        let _ = self.connection.destroy_window(self.window);
        let _ = self.connection.flush();
    }
}
