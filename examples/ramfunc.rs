//! examples/ramfunc.rs

#![deny(unsafe_code)]
#![deny(warnings)]
#![no_main]
#![no_std]

extern crate panic_semihosting;

use cortex_m_semihosting::debug;
use rtfm::app;

macro_rules! println {
    ($($tt:tt)*) => {
        if let Ok(mut stdout) = cortex_m_semihosting::hio::hstdout() {
            use core::fmt::Write;

            writeln!(stdout, $($tt)*).ok();
        }
    };
}

#[app(device = lm3s6965)]
const APP: () = {
    #[init(spawn = [bar])]
    fn init() {
        spawn.bar().unwrap();
    }

    #[inline(never)]
    #[task]
    fn foo() {
        println!("foo");

        debug::exit(debug::EXIT_SUCCESS);
    }

    #[inline(never)]
    #[link_section = ".data"]
    #[task(priority = 2, spawn = [foo])]
    fn bar() {
        spawn.foo().ok();
    }

    extern "C" {
        fn UART0();
        fn UART1();
    }
};
