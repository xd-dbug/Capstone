use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};
use conquer_once::spin::OnceCell;
use core::fmt;
use noto_sans_mono_bitmap::{get_raster, get_raster_width, FontWeight, RasterHeight};
use spin::Mutex;

const FONT_WEIGHT: FontWeight = FontWeight::Regular;
const CHAR_RASTER_HEIGHT: RasterHeight = RasterHeight::Size16;
const CHAR_HEIGHT: usize = CHAR_RASTER_HEIGHT.val();
const CHAR_WIDTH: usize = get_raster_width(FONT_WEIGHT, CHAR_RASTER_HEIGHT);

static WRITER: OnceCell<Mutex<Writer>> = OnceCell::uninit();

pub struct Writer {
    buffer: &'static mut [u8],
    info: FrameBufferInfo,
    x: usize,
    y: usize,
}

impl Writer {
    fn write_pixel(&mut self, x: usize, y: usize, intensity: u8) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        let byte_offset = (y * self.info.stride + x) * self.info.bytes_per_pixel;
        let pixel_bytes = &mut self.buffer[byte_offset..byte_offset + self.info.bytes_per_pixel];
        match self.info.pixel_format {
            PixelFormat::Rgb | PixelFormat::Bgr => {
                pixel_bytes[0] = intensity;
                pixel_bytes[1] = intensity;
                pixel_bytes[2] = intensity;
            }
            PixelFormat::U8 => {
                pixel_bytes[0] = intensity;
            }
            _ => {
                pixel_bytes[0] = intensity;
            }
        }
    }

    fn clear(&mut self) {
        self.buffer.fill(0);
        self.y = 0;
    }

    fn newline(&mut self) {
        self.x = 0;
        if self.y + CHAR_HEIGHT * 2 > self.info.height {
            self.clear();
        } else {
            self.y += CHAR_HEIGHT;
        }
    }

    pub fn write_char(&mut self, c: char) {
        match c {
            '\n' => self.newline(),
            '\r' => {}
            c => {
                if self.x + CHAR_WIDTH > self.info.width {
                    self.newline();
                }
                let glyph = get_raster(c, FONT_WEIGHT, CHAR_RASTER_HEIGHT)
                    .unwrap_or_else(|| get_raster('?', FONT_WEIGHT, CHAR_RASTER_HEIGHT).unwrap());
                for (row_i, row) in glyph.raster().iter().enumerate() {
                    for (col_i, intensity) in row.iter().enumerate() {
                        self.write_pixel(self.x + col_i, self.y + row_i, *intensity);
                    }
                }
                self.x += CHAR_WIDTH;
            }
        }
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            self.write_char(c);
        }
        Ok(())
    }
}

pub fn init(framebuffer: &'static mut FrameBuffer) {
    let info = framebuffer.info();
    let buffer = framebuffer.buffer_mut();
    let mut writer = Writer {
        buffer,
        info,
        x: 0,
        y: 0,
    };
    writer.clear();
    WRITER.init_once(|| Mutex::new(writer));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    if let Some(writer) = WRITER.get() {
        writer.lock().write_fmt(args).ok();
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::framebuffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[test_case]
fn test_println_simple() {
    println!("test_println_simple output");
}

#[test_case]
fn test_println_many() {
    for _ in 0..200 {
        println!("test_println_many output");
    }
}