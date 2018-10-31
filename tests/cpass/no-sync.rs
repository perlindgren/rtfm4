//! `Send` is not required for messages between cooperative (same priority) tasks
#![no_main]
#![no_std]

extern crate lm3s6965;
extern crate panic_halt;
extern crate rtfm;

use core::marker::PhantomData;

use rtfm::app;

struct NotSync {
    _0: PhantomData<*const ()>,
}

unsafe impl Send for NotSync {}

#[app(device = lm3s6965)]
const APP: () = {
    static X: NotSync = NotSync { _0: PhantomData };

    #[init]
    fn init() {}

    #[task(resources = [X])]
    fn foo() {
        let _: &NotSync = resources.X;
    }

    #[task(resources = [X])]
    fn bar() {
        let _: &NotSync = resources.X;
    }

    extern "C" {
        fn UART0();
    }
};
