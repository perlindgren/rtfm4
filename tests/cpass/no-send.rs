//! `Send` is not required for messages between cooperative (same priority) tasks
#![feature(extern_crate_item_prelude)] // ???
#![no_main]
#![no_std]

extern crate lm3s6965;
extern crate panic_halt;
extern crate rtfm;

use core::marker::PhantomData;

use rtfm::app;

pub struct NotSend {
    _0: PhantomData<*const ()>,
}

unsafe impl Sync for NotSend {}

#[app(device = lm3s6965)]
const APP: () = {
    #[init]
    fn init() {}

    #[task(spawn = [bar])]
    fn foo() {
        spawn.bar(NotSend { _0: PhantomData }).ok();
    }

    #[task]
    fn bar(_x: NotSend) {}

    extern "C" {
        fn UART0();
    }
};
