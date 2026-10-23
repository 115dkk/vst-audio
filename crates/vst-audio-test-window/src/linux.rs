use std::cell::Cell;
use vst_audio_protocol::{EditorImage, ParentWindow};
use x11rb::{
    connection::Connection, protocol::xproto::*, rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

#[derive(Debug)]
pub struct ResourceManager {
    connection: RustConnection,
    root: u32,
    atom: u32,
    previous: Option<(u32, u8, Vec<u8>)>,
}
impl ResourceManager {
    /// Replaces the root RESOURCE_MANAGER string until this guard drops.
    pub fn set(value: Option<&str>) -> Self {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let (connection, screen) = loop {
            match x11rb::connect(None) {
                Ok(connection) => break connection,
                Err(error) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "X11 connection: {error}"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        };
        let root = connection.setup().roots[screen].root;
        let atom = connection
            .intern_atom(false, b"RESOURCE_MANAGER")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        let reply = connection
            .get_property(false, root, atom, AtomEnum::ANY, 0, u32::MAX)
            .unwrap()
            .reply()
            .unwrap();
        let previous = (reply.type_ != 0).then_some((reply.type_, reply.format, reply.value));
        match value {
            Some(value) => connection
                .change_property8(
                    PropMode::REPLACE,
                    root,
                    atom,
                    AtomEnum::STRING,
                    value.as_bytes(),
                )
                .unwrap()
                .check()
                .unwrap(),
            None => connection
                .delete_property(root, atom)
                .unwrap()
                .check()
                .unwrap(),
        }
        connection.flush().unwrap();
        Self {
            connection,
            root,
            atom,
            previous,
        }
    }
}
impl Drop for ResourceManager {
    fn drop(&mut self) {
        let result = if let Some((type_, format, value)) = &self.previous {
            let unit = usize::from(*format) / 8;
            self.connection.change_property(
                PropMode::REPLACE,
                self.root,
                self.atom,
                *type_,
                *format,
                u32::try_from(value.len() / unit).unwrap(),
                value,
            )
        } else {
            self.connection.delete_property(self.root, self.atom)
        };
        if let Ok(cookie) = result {
            let _ = cookie.check();
        }
        let _ = self.connection.flush();
    }
}

#[derive(Debug)]
pub struct Parent {
    connection: RustConnection,
    window: u32,
    destroyed: Cell<bool>,
}
impl Parent {
    pub fn create(width: u32, height: u32) -> Self {
        // Xvfb resets when its last client disconnects. A new test can race that
        // reset; retry connection establishment only, never a failed assertion.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let (connection, screen) = loop {
            match x11rb::connect(None) {
                Ok(connection) => break connection,
                Err(error) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "X11 connection: {error}"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        };
        let root = connection.setup().roots[screen].root;
        let window = connection.generate_id().unwrap();
        connection
            .create_window(
                0,
                window,
                root,
                0,
                0,
                width.try_into().unwrap(),
                height.try_into().unwrap(),
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new().background_pixel(0xffffff),
            )
            .unwrap()
            .check()
            .unwrap();
        connection.map_window(window).unwrap().check().unwrap();
        connection.flush().unwrap();
        let parent = Self {
            connection,
            window,
            destroyed: Cell::new(false),
        };
        parent.wait_visible(true);
        parent
    }
    fn wait_visible(&self, visible: bool) {
        // A desktop WM handles MapWindow asynchronously; request acceptance is
        // not evidence that the parent is mapped. Do not assume a particular size.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            self.pump();
            let attributes = self
                .connection
                .get_window_attributes(self.window)
                .unwrap()
                .reply()
                .unwrap();
            if (attributes.map_state == MapState::VIEWABLE) == visible {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "parent mapping timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    pub fn handle(&self) -> ParentWindow {
        ParentWindow::X11 {
            window: self.window.into(),
        }
    }
    pub fn pump(&self) {
        while self.connection.poll_for_event().unwrap().is_some() {}
    }
    pub fn expect_input_process(&self, _pid: u32) {}
    pub(crate) fn pointer(&self, x: i32, y: i32, button: Option<bool>) {
        use x11rb::protocol::xtest::ConnectionExt as _;
        let root = self
            .connection
            .get_geometry(self.window)
            .unwrap()
            .reply()
            .unwrap()
            .root;
        let point = self
            .connection
            .translate_coordinates(
                self.window,
                root,
                x.try_into().unwrap(),
                y.try_into().unwrap(),
            )
            .unwrap()
            .reply()
            .unwrap();
        self.connection
            .xtest_fake_input(MOTION_NOTIFY_EVENT, 0, 0, root, point.dst_x, point.dst_y, 0)
            .unwrap()
            .check()
            .unwrap();
        if let Some(press) = button {
            self.connection
                .xtest_fake_input(
                    if press {
                        BUTTON_PRESS_EVENT
                    } else {
                        BUTTON_RELEASE_EVENT
                    },
                    1,
                    0,
                    root,
                    0,
                    0,
                    0,
                )
                .unwrap()
                .check()
                .unwrap();
        }
        // A reply is a server round trip; pumping also gives the foreign client time to run.
        self.connection.get_input_focus().unwrap().reply().unwrap();
        self.settle();
    }
    pub fn children(&self) -> Vec<(u64, Option<u32>)> {
        self.connection
            .query_tree(self.window)
            .unwrap()
            .reply()
            .unwrap()
            .children
            .into_iter()
            .map(|id| (id.into(), None))
            .collect()
    }
    /// Describes the top-level window that owns a parent client point on screen.
    pub fn window_at(&self, x: i32, y: i32) -> String {
        let root = self
            .connection
            .get_geometry(self.window)
            .unwrap()
            .reply()
            .unwrap()
            .root;
        let child = self
            .connection
            .translate_coordinates(
                self.window,
                root,
                x.try_into().unwrap(),
                y.try_into().unwrap(),
            )
            .unwrap()
            .reply()
            .unwrap()
            .child;
        format!("{child:#x} (parent {})", child == self.window)
    }
    pub fn capture(&self) -> EditorImage {
        let geometry = self
            .connection
            .get_geometry(self.window)
            .unwrap()
            .reply()
            .unwrap();
        let image = self
            .connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.window,
                0,
                0,
                geometry.width,
                geometry.height,
                u32::MAX,
            )
            .unwrap()
            .reply()
            .unwrap();
        let setup = self.connection.setup();
        let format = setup
            .pixmap_formats
            .iter()
            .find(|f| f.depth == image.depth)
            .unwrap();
        assert_eq!(format.bits_per_pixel, 32);
        let visual = setup
            .roots
            .iter()
            .flat_map(|s| &s.allowed_depths)
            .flat_map(|d| &d.visuals)
            .find(|v| v.visual_id == image.visual)
            .unwrap();
        assert_eq!(visual.class, VisualClass::TRUE_COLOR);
        let width = usize::from(geometry.width);
        let height = usize::from(geometry.height);
        let stride = (width * 32).div_ceil(format.scanline_pad as usize)
            * (format.scanline_pad as usize / 8);
        let mut rgba = vec![255; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let bytes = image.data[y * stride + x * 4..y * stride + x * 4 + 4]
                    .try_into()
                    .unwrap();
                let pixel = if setup.image_byte_order == ImageOrder::LSB_FIRST {
                    u32::from_le_bytes(bytes)
                } else {
                    u32::from_be_bytes(bytes)
                };
                for (index, mask) in [visual.red_mask, visual.green_mask, visual.blue_mask]
                    .into_iter()
                    .enumerate()
                {
                    assert_ne!(mask, 0);
                    rgba[(y * width + x) * 4 + index] =
                        (((pixel & mask) >> mask.trailing_zeros()) * 255
                            / (mask >> mask.trailing_zeros())) as u8;
                }
            }
        }
        EditorImage {
            width: width as u32,
            height: height as u32,
            rgba,
        }
    }
    pub fn set_visible(&self, visible: bool) {
        if visible {
            self.connection
                .map_window(self.window)
                .unwrap()
                .check()
                .unwrap();
        } else {
            self.connection
                .unmap_window(self.window)
                .unwrap()
                .check()
                .unwrap();
        }
        self.connection.flush().unwrap();
        self.wait_visible(visible);
    }
    pub fn destroy(&self) {
        if !self.destroyed.replace(true) {
            self.connection
                .destroy_window(self.window)
                .unwrap()
                .check()
                .unwrap();
            self.connection.flush().unwrap();
        }
    }
}
impl Drop for Parent {
    fn drop(&mut self) {
        self.destroy();
    }
}
