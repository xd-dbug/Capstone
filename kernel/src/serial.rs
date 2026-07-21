use conquer_once::spin::OnceCell;
use core::fmt;
use spin::Mutex;
use uart_16550::{backend::PioBackend, Config, Uart16550Tty};

static SERIAL1: OnceCell<Mutex<Uart16550Tty<PioBackend>>> = OnceCell::uninit();

fn init_serial() -> Mutex<Uart16550Tty<PioBackend>> {
    // 0x3F8 is the standard I/O port for the first serial interface (COM1).
    let uart = unsafe {
        Uart16550Tty::new_port(0x3F8, Config::default()).expect("failed to initialize UART")
    };
    Mutex::new(uart)
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    SERIAL1
        .get_or_init(init_serial)
        .lock()
        .write_fmt(args)
        .expect("printing to serial failed");
}

/// Prints to the host through the serial interface.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*));
    };
}

/// Prints to the host through the serial interface, appending a newline.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::serial_print!(
        concat!($fmt, "\n"), $($arg)*));
}