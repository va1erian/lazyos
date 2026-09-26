//! `serial_print!`/`serial_println!` macros (logging over COM1).

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {{
        $crate::serial::_print(format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! serial_println {
    () => {
        $crate::serial_print!("\n")
    };
    ($($arg:tt)*) => {{
        $crate::serial_print!("{}\n", format_args!($($arg)*));
    }};
}
