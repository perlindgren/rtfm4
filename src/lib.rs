//! Real Time For the Masses (RTFM) framework for ARM Cortex-M microcontrollers
//!
//! The user level documentation can be found [here].
//!
//! [here]: ../../book/index.html

#![deny(missing_docs)]
#![deny(warnings)]
#![no_std]

use core::u8;
use core::{cell::Cell, cmp::Ordering, ops};

#[cfg(armv7m)]
use cortex_m::register::basepri;
use cortex_m::{
    interrupt::{self, Nr},
    peripheral::{CBP, CPUID, DCB, DWT, FPB, FPU, ITM, MPU, NVIC, SCB, TPIU},
};
pub use cortex_m_rtfm_macros::app;

#[doc(hidden)]
pub mod export;
#[doc(hidden)]
mod tq;

/// Core peripherals
///
/// This is `cortex_m::Peripherals` minus the peripherals that the RTFM runtime uses
#[allow(non_snake_case)]
pub struct Peripherals<'a> {
    /// Cache and branch predictor maintenance operations (not present on Cortex-M0 variants)
    pub CBP: CBP,

    /// CPUID
    pub CPUID: CPUID,

    /// Debug Control Block
    pub DCB: &'a mut DCB,

    // Data Watchpoint and Trace unit
    // pub DWT: DWT,
    /// Flash Patch and Breakpoint unit (not present on Cortex-M0 variants)
    pub FPB: FPB,

    /// Floating Point Unit (only present on `thumbv7em-none-eabihf`)
    pub FPU: FPU,

    /// Instrumentation Trace Macrocell (not present on Cortex-M0 variants)
    pub ITM: ITM,

    /// Memory Protection Unit
    pub MPU: MPU,

    // Nested Vector Interrupt Controller
    // pub NVIC: NVIC,
    /// System Control Block
    pub SCB: &'a mut SCB,

    // SysTick: System Timer
    // pub SYST: SYST,
    /// Trace Port Interface Unit (not present on Cortex-M0 variants)
    pub TPIU: TPIU,
}

/// A measurement of a monotonically nondecreasing clock. Opaque and useful only with `Duration`
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Instant(i32);

impl Instant {
    /// IMPLEMENTATION DETAIL. DO NOT USE
    #[doc(hidden)]
    pub fn artificial(timestamp: i32) -> Self {
        Instant(timestamp)
    }

    /// Returns an instant corresponding to "now"
    pub fn now() -> Self {
        Instant(DWT::get_cycle_count() as i32)
    }

    /// Returns the amount of time elapsed since this instant was created.
    pub fn elapsed(&self) -> Duration {
        Instant::now() - *self
    }

    /// Returns the amount of time elapsed from another instant to this one.
    pub fn duration_since(&self, earlier: Instant) -> Duration {
        let diff = self.0 - earlier.0;
        assert!(diff >= 0, "second instant is later than self");
        Duration(diff as u32)
    }
}

impl ops::AddAssign<Duration> for Instant {
    fn add_assign(&mut self, dur: Duration) {
        debug_assert!(dur.0 < (1 << 31));
        self.0 = self.0.wrapping_add(dur.0 as i32);
    }
}

impl ops::Add<Duration> for Instant {
    type Output = Self;

    fn add(mut self, dur: Duration) -> Self {
        self += dur;
        self
    }
}

impl ops::SubAssign<Duration> for Instant {
    fn sub_assign(&mut self, dur: Duration) {
        // XXX should this be a non-debug assertion?
        debug_assert!(dur.0 < (1 << 31));
        self.0 = self.0.wrapping_sub(dur.0 as i32);
    }
}

impl ops::Sub<Duration> for Instant {
    type Output = Self;

    fn sub(mut self, dur: Duration) -> Self {
        self -= dur;
        self
    }
}

impl ops::Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, other: Instant) -> Duration {
        self.duration_since(other)
    }
}

impl Ord for Instant {
    fn cmp(&self, rhs: &Self) -> Ordering {
        self.0.wrapping_sub(rhs.0).cmp(&0)
    }
}

impl PartialOrd for Instant {
    fn partial_cmp(&self, rhs: &Self) -> Option<Ordering> {
        Some(self.cmp(rhs))
    }
}

/// A `Duration` type to represent a span of time.
#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct Duration(u32);

impl ops::AddAssign for Duration {
    fn add_assign(&mut self, dur: Duration) {
        self.0 += dur.0;
    }
}

impl ops::Add<Duration> for Duration {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Duration(self.0 + other.0)
    }
}

impl ops::SubAssign for Duration {
    fn sub_assign(&mut self, rhs: Duration) {
        self.0 -= rhs.0;
    }
}

impl ops::Sub<Duration> for Duration {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Duration(self.0 - rhs.0)
    }
}

/// Adds the `cycles` method to the `u32` type
pub trait U32Ext {
    /// Converts the `u32` value into number of.cycles
    fn cycles(self) -> Duration;
}

impl U32Ext for u32 {
    fn cycles(self) -> Duration {
        Duration(self)
    }
}

/// Memory safe access to shared resources
///
/// In RTFM, locks are implemented as critical sections that prevent other tasks from *starting*.
/// These critical sections are implemented by temporarily increasing the dynamic priority (see
/// BASEPRI) of the execution context.
pub unsafe trait Mutex {
    /// Logical priority ceiling
    const CEILING: u8;
    #[doc(hidden)]
    const NVIC_PRIO_BITS: u8;
    /// Data protected by the mutex
    type Data: Send;

    /// IMPLEMENTATION DETAIL. DO NOT USE THIS METHOD
    #[doc(hidden)]
    unsafe fn priority(&self) -> &Cell<u8>;

    /// IMPLEMENTATION DETAIL. DO NOT USE THIS METHOD
    #[doc(hidden)]
    fn ptr(&self) -> *mut Self::Data;

    /// Creates a critical section and grants temporary access to the protected data
    #[inline(always)]
    #[cfg(armv7m)]
    fn lock<R, F>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut Self::Data) -> R,
    {
        unsafe {
            let current = self.priority().get();

            if self.priority().get() < Self::CEILING {
                if Self::CEILING == (1 << Self::NVIC_PRIO_BITS) {
                    self.priority().set(u8::MAX);
                    let r = interrupt::free(|_| f(&mut *self.ptr()));
                    self.priority().set(current);
                    r
                } else {
                    self.priority().set(Self::CEILING);
                    basepri::write(logical2hw(Self::CEILING, Self::NVIC_PRIO_BITS));
                    let r = f(&mut *self.ptr());
                    basepri::write(logical2hw(current, Self::NVIC_PRIO_BITS));
                    self.priority().set(current);
                    r
                }
            } else {
                f(&mut *self.ptr())
            }
        }
    }

    /// Creates a critical section and grants temporary access to the protected data
    #[cfg(not(armv7m))]
    fn lock<R, F>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut Self::Data) -> R,
    {
        unsafe {
            let current = self.priority().get();

            if self.priority().get() < Self::CEILING {
                self.priority().set(u8::MAX);
                let r = interrupt::free(|_| f(&mut *self.ptr()));
                self.priority().set(current);
                r
            } else {
                f(&mut *self.ptr())
            }
        }
    }
}

#[cfg(armv7m)]
#[inline]
fn logical2hw(logical: u8, nvic_prio_bits: u8) -> u8 {
    ((1 << nvic_prio_bits) - logical) << (8 - nvic_prio_bits)
}

/// Sets the given `interrupt` as pending
///
/// This is a convenience function around `NVIC::pend`
pub fn pend<I>(interrupt: I)
where
    I: Nr,
{
    NVIC::pend(interrupt)
}