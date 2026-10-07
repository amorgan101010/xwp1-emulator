//! Casio XW-P1 emulator core. The firmware runs on an ARM7TDMI interpreter
//! (`arm`; Unicorn is kept as its reference) against models of the
//! uPD800468's peripherals and sound source, stepped one audio sample at a
//! time. Evidence for everything modelled here is in
//! docs/FINDINGS.md; `emu/*.py` is the reference this was ported from.
pub mod arm;
pub mod card;
pub mod engine;
pub mod flash;
pub mod front;
pub mod image;
pub mod machine;
pub mod macro_lfo;
pub mod panel;
pub mod poly;
pub mod reverb;
pub mod soc;
pub mod sound;
pub mod vary;
pub mod setup;
pub mod waves;

#[cfg(feature = "python")]
mod py;
