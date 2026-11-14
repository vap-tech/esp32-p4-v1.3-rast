//! Cooperative board UI. No filesystem access and no nested event loop.
use core::fmt::Write;
use esp32_p4::{
    clock_face::{self, ClockFace},
    devices::{
        es8311::Es8311,
        framebuffer::Framebuffer,
        gt911::{Gt911, Kind, Tracker},
        rx8025t::Rx8025t,
    },
    hal::{i2c::I2c, time::Timer, usb_serial::Console},
};
#[derive(Default, Debug)]
pub struct Stats {
    calls: u64,
    last: u64,
    touch_due: u64,
    rtc_reads: u32,
    clock_draws: u32,
    glyphs: u32,
    touch_polls: u32,
    rtc_errors: u32,
    max_gap_us: u64,
    max_work_us: u64,
}
pub trait Services {
    fn poll(&mut self, timer: &mut Timer, console: &mut Console);
    fn alarm_active(&self) -> bool;
    fn command(&mut self, command: &str, timer: &mut Timer, console: &mut Console) -> bool;
    fn reset_metrics(&mut self);
    fn codec(&mut self, timer: &mut Timer) -> Result<Es8311, esp32_p4::hal::i2c::Error>;
    fn mute(
        &mut self,
        codec: &mut Es8311,
        timer: &mut Timer,
        mute: bool,
    ) -> Result<(), esp32_p4::hal::i2c::Error>;
    fn report(&self, console: &mut Console);
}
pub struct Ui<'a> {
    pub bus: &'a mut I2c,
    pub touch: &'a mut Gt911,
    pub tracker: &'a mut Tracker,
    pub framebuffer: Option<&'a mut Framebuffer>,
    pub face: &'a mut ClockFace,
    pub clock_enabled: &'a mut bool,
    pub active: &'a mut bool,
    pub alarm_retry: &'a mut u64,
    pub rtc_due: &'a mut u64,
    pub clock_due: &'a mut u64,
    pub frames: &'a mut u32,
    pub errors: &'a mut u32,
    pub stats: &'a mut Stats,
}
impl Services for Ui<'_> {
    fn poll(&mut self, timer: &mut Timer, console: &mut Console) {
        let start = timer.now_us();
        if self.stats.last != 0 {
            self.stats.max_gap_us = self
                .stats
                .max_gap_us
                .max(start.wrapping_sub(self.stats.last));
        }
        self.stats.last = start;
        self.stats.calls += 1;
        let rtc_due = start.wrapping_sub(*self.rtc_due) >= 1_000_000;
        let clock_due = *self.clock_enabled
            && self.framebuffer.is_some()
            && start.wrapping_sub(*self.clock_due) >= 1_000_000;
        if rtc_due || clock_due {
            *self.rtc_due = start;
            self.stats.rtc_reads += 1;
            let snapshot = Rx8025t::read(self.bus, timer);
            if snapshot.is_err() {
                self.stats.rtc_errors += 1;
            }
            if let Ok(s) = snapshot.as_ref() {
                if s.time_usable() && s.alarm_enabled() && s.alarm_pending() && !*self.active {
                    *self.active = true;
                    *self.alarm_retry = timer.now_us();
                    *self.clock_enabled = true;
                    let _ = writeln!(console, "ALARM FIRED RTC AF=true AIE=true");
                    if let Some(fb) = self.framebuffer.as_mut() {
                        clock_face::alarm_button(fb, true);
                    }
                }
            }
            if clock_due {
                *self.clock_due = start;
                let date = snapshot
                    .ok()
                    .filter(|s| s.time_usable())
                    .and_then(|s| s.datetime().ok());
                if let Some(fb) = self.framebuffer.as_mut() {
                    self.stats.glyphs += self.face.draw(fb, date) as u32;
                    self.stats.clock_draws += 1;
                }
            }
        }
        if start.wrapping_sub(self.stats.touch_due) >= 20_000 {
            self.stats.touch_due = start;
            self.stats.touch_polls += 1;
            match self.touch.read_frame(self.bus, timer) {
                Ok(frame) => {
                    if frame.is_some() {
                        *self.frames = self.frames.wrapping_add(1);
                    }
                    for event in self.tracker.update(frame).into_iter().flatten() {
                        if *self.active
                            && event.kind == Kind::Down
                            && clock_face::stop_hit(event.point.x, event.point.y)
                        {
                            match Rx8025t::acknowledge_alarm(self.bus, timer) {
                                Ok(()) => {
                                    *self.active = false;
                                    if let Some(fb) = self.framebuffer.as_mut() {
                                        clock_face::alarm_button(fb, false);
                                    }
                                    let _ =
                                        writeln!(console, "ALARM ACK RTC AF cleared; AIE retained");
                                }
                                Err(error) => {
                                    let _ = writeln!(console, "ALARM ACK ERROR {:?}", error);
                                }
                            }
                        }
                        let _ = writeln!(
                            console,
                            "touch {:?} id={} x={} y={} size={} us={}",
                            event.kind,
                            event.point.id,
                            event.point.x,
                            event.point.y,
                            event.point.size,
                            event.timestamp_us
                        );
                    }
                }
                Err(error) => {
                    *self.errors = self.errors.wrapping_add(1);
                    if *self.errors % 100 == 1 {
                        let _ = writeln!(console, "GT911 error: {:?}", error);
                    }
                }
            }
        }
        self.stats.max_work_us = self
            .stats
            .max_work_us
            .max(timer.now_us().wrapping_sub(start));
    }
    fn command(&mut self, command: &str, timer: &mut Timer, console: &mut Console) -> bool {
        if let Some(command) = command.strip_prefix("rtc ") {
            let result = crate::rtc_command(command, self.bus, timer);
            match result {
                Ok(ref s) => {
                    if *self.active && (!s.alarm_enabled() || !s.alarm_pending()) {
                        *self.active = false;
                        if let Some(fb) = self.framebuffer.as_mut() {
                            clock_face::alarm_button(fb, false);
                        }
                    }
                    *self.rtc_due = timer.now_us().wrapping_sub(1_000_000);
                    let _ = writeln!(
                        console,
                        "RTC DATA date={:?} usable={} alarm={} AIE={} VLF={} VDET={}",
                        s.datetime(),
                        s.time_usable(),
                        s.alarm_pending(),
                        s.alarm_enabled(),
                        s.voltage_low(),
                        s.voltage_detected()
                    );
                }
                Err(ref error) => {
                    let _ = writeln!(console, "RTC ERROR {:?}", error);
                }
            }
            let _ = writeln!(console, "RTC DONE {:?}", result.map(|_| ()));
            return true;
        }
        if matches!(command, "display clock on" | "display clock off") {
            *self.clock_enabled = command.ends_with(" on");
            self.face.invalidate();
            *self.clock_due = timer.now_us().wrapping_sub(1_000_000);
            let _ = writeln!(console, "DISPLAY CLOCK DONE Ok({})", self.clock_enabled);
            return true;
        }
        false
    }
    fn reset_metrics(&mut self) {
        *self.stats = Stats::default();
    }
    fn alarm_active(&self) -> bool {
        *self.active
    }
    fn codec(&mut self, timer: &mut Timer) -> Result<Es8311, esp32_p4::hal::i2c::Error> {
        Es8311::new(self.bus, timer)
    }
    fn mute(
        &mut self,
        codec: &mut Es8311,
        timer: &mut Timer,
        mute: bool,
    ) -> Result<(), esp32_p4::hal::i2c::Error> {
        codec.mute(self.bus, timer, mute)
    }
    fn report(&self, console: &mut Console) {
        let _ = writeln!(console, "BACKGROUND UI {:?}", self.stats);
    }
}
