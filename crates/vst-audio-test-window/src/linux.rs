use std::cell::Cell;
use vst_audio_protocol::{EditorImage, ParentWindow};
use x11rb::{connection::Connection, protocol::xproto::*, rust_connection::RustConnection};

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
        Self {
            connection,
            window,
            destroyed: Cell::new(false),
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
