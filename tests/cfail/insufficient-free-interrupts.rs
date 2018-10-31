#![no_main]
#![no_std]

extern crate lm3s6965;
extern crate panic_halt;
extern crate rtfm;

use core::marker::PhantomData;

use rtfm::app;

#[app(device = lm3s6965)] //~ ERROR 1 free interrupt (`extern { .. }`) is required
const APP: () = {
    #[init]
    fn init() {}

    #[task]
    fn foo() {}
};
