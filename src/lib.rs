#![feature(coroutines, coroutine_trait, stmt_expr_attributes)]

// BUGS not found by the harte tests:
// - jmp abs
// - branch with -128 relative offset
// - php pushing B flag (obelisk guide doesn't mention) (also klaus
//   doesn't ensure that the B flag is set in the *pushed* status byte
//   but not in the actual current status register)
// - plp should not change the B flag (not found by klaus)

use std::ops::{Coroutine};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, Ordering};

use bitflags::bitflags;

bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct Flags: u8 {
        const C = 1 << 0;
        const Z = 1 << 1;
        const I = 1 << 2;
        const D = 1 << 3;
        const B = 1 << 4;
        const U = 1 << 5;
        const V = 1 << 6;
        const N = 1 << 7;
    }
}

#[derive(Clone, Debug)]
pub struct Cpu {
    pub pc: u16,
    pub sp: u8,
    pub a: u8,
    pub x: u8,
    pub y: u8,

    pub status: Flags,
    pub apu_status: Flags,
    pub apu_enabled: bool,

    pub reset_signal: Arc<AtomicBool>,
    pub irq_signal: Arc<AtomicBool>,
    pub nmi_signal: Arc<AtomicBool>,

    pub io_bus: Arc<AtomicU8>,

    // Only updates at the start of each new instruction.
    pub pc_monitor: Arc<AtomicU16>,
}

impl fmt::Display for Cpu {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "pc: {:04x}", self.pc)?;
        writeln!(f, "sp: {:02x}", self.sp)?;
        writeln!(f, "a: {:02x}", self.a)?;
        writeln!(f, "x: {:02x}", self.x)?;
        writeln!(f, "y: {:02x}", self.y)?;
        writeln!(f, "p: {:08b}", self.status)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IO {
    Read,
    Write,
}

impl IO {
    pub fn is_read(&self) -> bool {
        match self {
            IO::Read => true,
            IO::Write => false,
        }
    }
}

// Every CPU event contains an address and either reads from that
// address or writes a byte to it.
pub type CpuEvent = (u16, Option<IO>);

// impl fmt::Display for CpuEvent {
//     fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
//  match self {
//      CpuEvent::Read(adr) => write!(f, "Read({:04x})", adr),
//      CpuEvent::Write(adr, val) =>
//      write!(f, "Write({:04x}, {:02x})", adr, val),
//  }
//     }
// }

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            pc: 0,
            sp: 0xFF,
            a: 0,
            x: 0,
            y: 0,
            // status: Flags::empty() | Flags::D | Flags::B,
            status: Flags::I | Flags::U,
            apu_status: Flags::empty(),
            apu_enabled: true,
            reset_signal: Arc::new(AtomicBool::new(false)),
            irq_signal: Arc::new(AtomicBool::new(false)),
            nmi_signal: Arc::new(AtomicBool::new(false)),
            io_bus: Arc::new(AtomicU8::new(0)),
            pc_monitor: Arc::new(AtomicU16::new(0)),
        }
    }
    fn set_flag(&mut self, f: Flags, b: bool) {
        self.status.set(f, b)
    }
    fn get_flag(&self, f: Flags) -> bool {
        self.status.contains(f)
    }
    fn reset(&mut self) {
        self.pc = 0;
        self.sp = 0xFF;
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.status = Flags::U;
        // Also set I after one read cycle -- see reset_signal handler code
        // self.io_bus.store(0, Ordering::Relaxed);
    }

    fn decode(op: u8) -> &'static Instr {
        const fn i(opcode: Opcode, mode: AddrMode) -> Instr {
            Instr { opcode, mode }
        }
        static INSTR_TABLE: [Instr; 256] = [
            i(Brk, Imp), i(Ora, Izx), i(Hlt, Imp), i(Slo, Izx), i(Nop, Zp0), i(Ora, Zp0), i(Asl, Zp0), i(Slo, Zp0), i(Php, Imp), i(Ora, Imm), i(Asl, Imp), i(Xxx, Imp), i(Nop, Abs), i(Ora, Abs), i(Asl, Abs), i(Slo, Abs),
            i(Bpl, Rel), i(Ora, Izy), i(Hlt, Imp), i(Slo, Izy), i(Nop, Zpx), i(Ora, Zpx), i(Asl, Zpx), i(Slo, Zpx), i(Clc, Imp), i(Ora, Aby), i(Nop, Imp), i(Slo, Aby), i(Nop, Abx), i(Ora, Abx), i(Asl, Abx), i(Slo, Abx),
            i(Jsr, Abs), i(And, Izx), i(Hlt, Imp), i(Rla, Izx), i(Bit, Zp0), i(And, Zp0), i(Rol, Zp0), i(Rla, Zp0), i(Plp, Imp), i(And, Imm), i(Rol, Imp), i(Xxx, Imp), i(Bit, Abs), i(And, Abs), i(Rol, Abs), i(Rla, Abs),
            i(Bmi, Rel), i(And, Izy), i(Hlt, Imp), i(Rla, Izy), i(Nop, Zpx), i(And, Zpx), i(Rol, Zpx), i(Rla, Zpx), i(Sec, Imp), i(And, Aby), i(Nop, Imp), i(Rla, Aby), i(Nop, Abx), i(And, Abx), i(Rol, Abx), i(Rla, Abx),
            i(Rti, Imp), i(Eor, Izx), i(Hlt, Imp), i(Sre, Izx), i(Nop, Zp0), i(Eor, Zp0), i(Lsr, Zp0), i(Sre, Zp0), i(Pha, Imp), i(Eor, Imm), i(Lsr, Imp), i(Xxx, Imp), i(Jmp, Abs), i(Eor, Abs), i(Lsr, Abs), i(Sre, Abs),
            i(Bvc, Rel), i(Eor, Izy), i(Hlt, Imp), i(Sre, Izy), i(Nop, Zpx), i(Eor, Zpx), i(Lsr, Zpx), i(Sre, Zpx), i(Cli, Imp), i(Eor, Aby), i(Nop, Imp), i(Sre, Aby), i(Nop, Abx), i(Eor, Abx), i(Lsr, Abx), i(Sre, Abx),
            i(Rts, Imp), i(Adc, Izx), i(Hlt, Imp), i(Rra, Izx), i(Nop, Zp0), i(Adc, Zp0), i(Ror, Zp0), i(Rra, Zp0), i(Pla, Imp), i(Adc, Imm), i(Ror, Imp), i(Xxx, Imp), i(Jmp, Ind), i(Adc, Abs), i(Ror, Abs), i(Rra, Abs),
            i(Bvs, Rel), i(Adc, Izy), i(Hlt, Imp), i(Rra, Izy), i(Nop, Zpx), i(Adc, Zpx), i(Ror, Zpx), i(Rra, Zpx), i(Sei, Imp), i(Adc, Aby), i(Nop, Imp), i(Rra, Aby), i(Nop, Abx), i(Adc, Abx), i(Ror, Abx), i(Rra, Abx),
            i(Nop, Imm), i(Sta, Izx), i(Nop, Imm), i(Sax, Izx), i(Sty, Zp0), i(Sta, Zp0), i(Stx, Zp0), i(Sax, Zp0), i(Dey, Imp), i(Nop, Imm), i(Txa, Imp), i(Xxx, Imp), i(Sty, Abs), i(Sta, Abs), i(Stx, Abs), i(Sax, Abs),
            i(Bcc, Rel), i(Sta, Izy), i(Hlt, Imp), i(Xxx, Imp), i(Sty, Zpx), i(Sta, Zpx), i(Stx, Zpy), i(Sax, Zpy), i(Tya, Imp), i(Sta, Aby), i(Txs, Imp), i(Xxx, Imp), i(Xxx, Imp), i(Sta, Abx), i(Xxx, Imp), i(Xxx, Imp),
            i(Ldy, Imm), i(Lda, Izx), i(Ldx, Imm), i(Lax, Izx), i(Ldy, Zp0), i(Lda, Zp0), i(Ldx, Zp0), i(Lax, Zp0), i(Tay, Imp), i(Lda, Imm), i(Tax, Imp), i(Lax, Imm), i(Ldy, Abs), i(Lda, Abs), i(Ldx, Abs), i(Lax, Abs),
            i(Bcs, Rel), i(Lda, Izy), i(Hlt, Imp), i(Lax, Izy), i(Ldy, Zpx), i(Lda, Zpx), i(Ldx, Zpy), i(Lax, Zpy), i(Clv, Imp), i(Lda, Aby), i(Tsx, Imp), i(Xxx, Imp), i(Ldy, Abx), i(Lda, Abx), i(Ldx, Aby), i(Lax, Aby),
            i(Cpy, Imm), i(Cmp, Izx), i(Nop, Imm), i(Dcp, Izx), i(Cpy, Zp0), i(Cmp, Zp0), i(Dec, Zp0), i(Dcp, Zp0), i(Iny, Imp), i(Cmp, Imm), i(Dex, Imp), i(Xxx, Imp), i(Cpy, Abs), i(Cmp, Abs), i(Dec, Abs), i(Dcp, Abs),
            i(Bne, Rel), i(Cmp, Izy), i(Hlt, Imp), i(Dcp, Izy), i(Nop, Zpx), i(Cmp, Zpx), i(Dec, Zpx), i(Dcp, Zpx), i(Cld, Imp), i(Cmp, Aby), i(Nop, Imp), i(Dcp, Aby), i(Nop, Abx), i(Cmp, Abx), i(Dec, Abx), i(Dcp, Abx),
            i(Cpx, Imm), i(Sbc, Izx), i(Nop, Imm), i(Isc, Izx), i(Cpx, Zp0), i(Sbc, Zp0), i(Inc, Zp0), i(Isc, Zp0), i(Inx, Imp), i(Sbc, Imm), i(Nop, Imp), i(Sbc, Imm), i(Cpx, Abs), i(Sbc, Abs), i(Inc, Abs), i(Isc, Abs),
            i(Beq, Rel), i(Sbc, Izy), i(Hlt, Imp), i(Isc, Izy), i(Nop, Zpx), i(Sbc, Zpx), i(Inc, Zpx), i(Isc, Zpx), i(Sed, Imp), i(Sbc, Aby), i(Nop, Imp), i(Isc, Aby), i(Nop, Abx), i(Sbc, Abx), i(Inc, Abx), i(Isc, Abx),
        ];
        &INSTR_TABLE[op as usize]
    }

    pub fn update_monitor(&self) {
        self.pc_monitor.store(self.pc, Ordering::Relaxed);
    }

    pub fn run(&mut self)
               -> Box<dyn Coroutine<Yield = CpuEvent, Return = String> + Unpin + '_> {

        macro_rules! fetch {
            ( $adr:expr ) => {
                {
                    yield ($adr, Some(IO::Read));
                    self.io_bus.load(Ordering::Relaxed)
                }
            };
        }

        macro_rules! write {
            ( $adr:expr, $val:expr ) => {
                {
                    self.io_bus.store($val, Ordering::Relaxed);
                    yield ($adr, Some(IO::Write))
                }
            };
        }

        macro_rules! next_pc {
            ( ) => {
                {
                    yield (self.pc, Some(IO::Read));
                    self.pc = self.pc.wrapping_add(1);
                    self.io_bus.load(Ordering::Relaxed)
                }
            };
        }

        macro_rules! push {
            ( $x:expr ) => {
                {
                    write!(0x0100 + self.sp as u16, $x);
                    self.sp = self.sp.wrapping_sub(1)
                }
            };
        }

        // Maybe shouldn't call this peek since it reads the byte past
        // the end of the stack.
        macro_rules! peek {
            ( ) => {
                {
                    fetch!(0x0100 + self.sp as u16)
                }
            };
        }

        macro_rules! pop {
            ( ) => {
                {
                    self.sp = self.sp.wrapping_add(1);
                    fetch!(0x0100 + self.sp as u16)
                }
            };
        }

        // Branch to pc+rel if $flag is $b.
        macro_rules! branch {
            ( $rel:expr, $flag:expr, $b:expr ) => {
                if self.get_flag($flag) == $b {
                    let pcl = self.pc as u8;
                    let pch = self.pc & 0xFF00;
                    let _ = next_pc!();
                    if $rel >= 0 {
                        let (new_pcl, carry) = pcl.overflowing_add($rel as u8);
                        self.pc = pch | new_pcl as u16; // Mask unnecessary?
                        if carry {
                            let _ = fetch!(self.pc);
                            self.pc = self.pc.wrapping_add(256)
                        }
                    } else {
                        let (new_pcl, carry) = pcl.overflowing_sub($rel.unsigned_abs());
                        self.pc = pch | new_pcl as u16;
                        if carry {
                            let _ = fetch!(self.pc);
                            self.pc = self.pc.wrapping_sub(256)
                        }
                    }
                }
            };
        }

        let go = #[coroutine] move || {
            let mut delayed_i_flag: Option<bool> = None;
            loop {
                self.update_monitor();

                // Poll reset/interrupt signals
                if self.reset_signal.load(Ordering::Relaxed) {
                    // Wait for signal to be cleared to actually proceed?
                    // while self.reset_signal.load(Ordering::Relaxed) {
                    //  cycle!()
                    // }
                    let _ = fetch!(0x00FF);
                    let _ = fetch!(0x0100);
                    self.reset();
                    let _ = pop!();
                    let _ = pop!();
                    let _ = pop!();
                    let pcl = fetch!(0xFFFC) as u16;
                    self.set_flag(Flags::I, true);
                    let pch = fetch!(0xFFFD) as u16;
                    self.pc = (pch << 8) | pcl;
                    self.reset_signal.store(false, Ordering::Relaxed);
                }

                if self.nmi_signal.load(Ordering::Relaxed) {
                    // println!("NMI!");
                    let _ = fetch!(self.pc);
                    let _ = fetch!(self.pc);
                    self.set_flag(Flags::B, false);
                    push!(((self.pc & 0xFF00) >> 8) as u8); // Push PCH
                    push!((self.pc & 0x00FF) as u8); // Push PCL
                    self.set_flag(Flags::U, true);
                    push!(self.status.bits());
                    let pcl = fetch!(0xFFFA) as u16;
                    self.set_flag(Flags::I, true); // Conflicting documentation
                    let pch = fetch!(0xFFFB) as u16;
                    self.pc = (pch << 8) | pcl;
                    self.nmi_signal.store(false, Ordering::Relaxed);
                    continue
                }

                if self.irq_signal.load(Ordering::Relaxed) && !self.get_flag(Flags::I) {
                    // println!("IRQ!");
                    let _ = fetch!(self.pc);
                    let _ = fetch!(self.pc);
                    self.set_flag(Flags::B, false);
                    push!(((self.pc & 0xFF00) >> 8) as u8); // Push PCH
                    push!((self.pc & 0x00FF) as u8); // Push PCL
                    self.set_flag(Flags::U, true);
                    push!(self.status.bits());
                    if self.nmi_signal.load(Ordering::Relaxed) {
                        let pcl = fetch!(0xFFFA) as u16;
                        self.set_flag(Flags::I, true); // Conflicting documentation
                        let pch = fetch!(0xFFFB) as u16;
                        self.pc = (pch << 8) | pcl;
                        self.nmi_signal.store(false, Ordering::Relaxed);
                    } else {
                        let pcl = fetch!(0xFFFE) as u16;
                        self.set_flag(Flags::I, true);
                        let pch = fetch!(0xFFFF) as u16;
                        self.pc = (pch << 8) | pcl;
                        self.irq_signal.store(false, Ordering::Relaxed);
                    }
                    continue
                }
                let instr_code = next_pc!();
                let instr = Self::decode(instr_code);

                if let Some(b) = delayed_i_flag {
                    self.set_flag(Flags::I, b);
                    delayed_i_flag = None
                }

                let mut lo_carry = false;
                let mut rel: i8 = 0;
                let fetch_addr: u16 = match instr.mode {
                    AddrMode::Imp => {
                        let _ = fetch!(self.pc);
                        0
                    }
                    AddrMode::Imm => {
                        let adr = self.pc;
                        self.pc = self.pc.wrapping_add(1);
                        adr
                    }
                    AddrMode::Zp0 => next_pc!().into(),
                    AddrMode::Zpx => {
                        let adr = next_pc!();
                        let _ = fetch!(adr as u16);
                        (adr.wrapping_add(self.x)).into()
                    }
                    AddrMode::Zpy => {
                        let adr = next_pc!();
                        let _ = fetch!(adr as u16);
                        (adr.wrapping_add(self.y)).into()
                    }
                    AddrMode::Rel => {
                        rel = next_pc!() as i8;
                        0
                    }
                    AddrMode::Abs => match instr.opcode {
                        Jsr => 0,
                        _ => {
                            let (lo, hi) = (next_pc!(), next_pc!());
                            ((hi as u16) << 8) | lo as u16
                        }
                    }
                    AddrMode::Abx => {
                        let (lo, hi) = (next_pc!(), next_pc!());
                        let (lo2, carry) = lo.overflowing_add(self.x);
                        lo_carry = carry;
                        ((hi as u16) << 8) | lo2 as u16
                    }
                    AddrMode::Aby => {
                        let (lo, hi) = (next_pc!(), next_pc!());
                        let (lo2, carry) = lo.overflowing_add(self.y);
                        lo_carry = carry;
                        ((hi as u16) << 8) | lo2 as u16
                    }
                    AddrMode::Ind => {
                        let (lo, hi) = (next_pc!(), next_pc!());
                        let loc = (hi as u16) << 8 | lo as u16;
                        let ptr_lo = fetch!(loc);
                        let ptr_hi = fetch!(if lo == 0x00FF { // Page boundary bug
                            loc & 0xFF00
                        } else {
                            loc + 1
                        });
                        ((ptr_hi as u16) << 8) | ptr_lo as u16
                    }
                    AddrMode::Izx => {
                        let t = next_pc!();
                        let _ = fetch!(t as u16);
                        let lo = fetch!(t.wrapping_add(self.x) as u16);
                        let hi = fetch!(t.wrapping_add(self.x).wrapping_add(1) as u16);
                        ((hi as u16) << 8) | lo as u16
                    }
                    AddrMode::Izy => {
                        let t = next_pc!();
                        let lo = fetch!(t as u16);
                        let hi = fetch!(t.wrapping_add(1) as u16);
                        let (lo2, carry) = lo.overflowing_add(self.y);
                        lo_carry = carry;
                        ((hi as u16) << 8) | lo2 as u16
                    }
                };

                let corrected_addr = if lo_carry {
                    fetch_addr.wrapping_add(256)
                } else {
                    fetch_addr
                };

                // Does second fetch with lo_carry applied when necessary.
                macro_rules! fetch_oops {
                    ( ) => {
                        {
                            let val = fetch!(fetch_addr);
                            if lo_carry {
                                fetch!(corrected_addr)
                            } else {
                                val
                            }
                        }
                    };
                }

                // Does second fetch when addressing mode is Abx or
                // Aby regardless of whether lo_carry needs to be
                // applied (thus results in a totally redundant extra
                // read when lo_carry is false).
                macro_rules! fetch_oops_store {
                    ( $mode:expr ) => {
                        {
                            let val = fetch!(fetch_addr);
                            match $mode {
                                AddrMode::Abx | AddrMode::Aby | AddrMode::Izy => {
                                    fetch!(if lo_carry {
                                        fetch_addr.wrapping_add(256)
                                    } else {
                                        fetch_addr
                                    })
                                }
                                _ => val
                            }
                        }
                    };
                }

                match instr.opcode {
                    Nop => match instr.mode {
                        AddrMode::Imp => (),
                        _ => {
                            let _ = fetch_oops!();
                        }
                    }
                    And => {
                        let val = fetch_oops!();
                        self.a &= val;
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::N, self.a & 0x80 != 0);
                    }
                    Adc => {
                        let val = fetch_oops!();
                        let res =
                            val as u16 + self.a as u16 + self.get_flag(Flags::C) as u16;
                        let a_sign = self.a & 0b10000000;
                        let val_sign = val & 0b10000000;
                        let res_sign = res as u8 & 0b10000000;
                        let signed_overflow = a_sign == val_sign && a_sign != res_sign;
                        self.a = res as u8;
                        self.set_flag(Flags::C, res & 0xFF00 != 0);
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::V, signed_overflow);
                        self.set_flag(Flags::N, res_sign > 0);
                    }
                    Sbc => {
                        let val = fetch_oops!() ^ 0xFF;
                        let res =
                            val as u16 + self.a as u16 + self.get_flag(Flags::C) as u16;
                        let a_sign = self.a & 0b10000000;
                        let val_sign = val & 0b10000000;
                        let res_sign = res as u8 & 0b10000000;
                        let signed_overflow = a_sign == val_sign && a_sign != res_sign;
                        self.a = res as u8;
                        self.set_flag(Flags::C, res & 0xFF00 != 0);
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::V, signed_overflow);
                        self.set_flag(Flags::N, res_sign > 0);
                    }

                    Asl => {
                        match instr.mode {
                            AddrMode::Imp => {
                                let shifted_bit = self.a & 0b10000000;
                                self.a <<= 1;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                self.set_flag(Flags::Z, self.a == 0);
                                self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                            }
                            _ => {
                                let val = fetch_oops_store!(instr.mode);
                                write!(corrected_addr, val);
                                let shifted_bit = val & 0b10000000;
                                let res = val << 1;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                self.set_flag(Flags::Z, res == 0);
                                self.set_flag(Flags::N, res & 0b10000000 != 0);
                                write!(corrected_addr, res);
                            }
                        }
                    }

                    Lsr => {
                        match instr.mode {
                            AddrMode::Imp => {
                                let shifted_bit = self.a & 1;
                                self.a >>= 1;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                self.set_flag(Flags::Z, self.a == 0);
                                self.set_flag(Flags::N, false);
                            }
                            _ => {
                                let val = fetch_oops_store!(instr.mode);
                                write!(corrected_addr, val);
                                let shifted_bit = val & 1;
                                let res = val >> 1;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                self.set_flag(Flags::Z, res == 0);
                                self.set_flag(Flags::N, false);
                                write!(corrected_addr, res);
                            }
                        }
                    }

                    Rol => {
                        match instr.mode {
                            AddrMode::Imp => {
                                let shifted_bit = self.a & 0b10000000;
                                self.a = (self.a << 1) | self.get_flag(Flags::C) as u8;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                self.set_flag(Flags::Z, self.a == 0);
                                self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                            }
                            _ => {
                                let val = fetch_oops_store!(instr.mode);
                                write!(corrected_addr, val);
                                let shifted_bit = val & 0b10000000;
                                let res = (val << 1) | self.get_flag(Flags::C) as u8;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                self.set_flag(Flags::Z, res == 0);
                                self.set_flag(Flags::N, res & 0b10000000 != 0);
                                write!(corrected_addr, res);
                            }
                        }
                    }

                    Ror => {
                        match instr.mode {
                            AddrMode::Imp => {
                                let shifted_bit = self.a & 1;
                                self.a =
                                    (self.get_flag(Flags::C) as u8) << 7 | self.a >> 1;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                self.set_flag(Flags::Z, self.a == 0);
                                self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                            }
                            _ => {
                                let val = fetch_oops_store!(instr.mode);
                                write!(corrected_addr, val);
                                let shifted_bit = val & 1;
                                let res =
                                    (self.get_flag(Flags::C) as u8) << 7 | val >> 1;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                self.set_flag(Flags::Z, res == 0);
                                self.set_flag(Flags::N, res & 0b10000000 != 0);
                                write!(corrected_addr, res);
                            }
                        }
                    }

                    Bit => {
                        let val = fetch!(fetch_addr);
                        self.set_flag(Flags::Z, self.a & val == 0);
                        self.set_flag(Flags::V, val & 0b01000000 != 0);
                        self.set_flag(Flags::N, val & 0b10000000 != 0);
                    }

                    Bcc => branch!(rel, Flags::C, false),
                    Bcs => branch!(rel, Flags::C, true),
                    Bne => branch!(rel, Flags::Z, false),
                    Beq => branch!(rel, Flags::Z, true),
                    Bpl => branch!(rel, Flags::N, false),
                    Bmi => branch!(rel, Flags::N, true),
                    Bvc => branch!(rel, Flags::V, false),
                    Bvs => branch!(rel, Flags::V, true),
                    Clc => self.set_flag(Flags::C, false),
                    Cld => self.set_flag(Flags::D, false),
                    Cli => self.set_flag(Flags::I, false),
                    Clv => self.set_flag(Flags::V, false),
                    Sec => self.set_flag(Flags::C, true),
                    Sed => self.set_flag(Flags::D, true),
                    Sei => self.set_flag(Flags::I, true),

                    Cmp => {
                        let val = fetch_oops!();
                        self.set_flag(Flags::C, self.a >= val);
                        self.set_flag(Flags::Z, self.a == val);
                        let (res, _) = self.a.overflowing_sub(val);
                        self.set_flag(Flags::N, res & 0b10000000 != 0);
                    }
                    Cpx => {
                        let val = fetch_oops!();
                        self.set_flag(Flags::C, self.x >= val);
                        self.set_flag(Flags::Z, self.x == val);
                        let (res, _) = self.x.overflowing_sub(val);
                        self.set_flag(Flags::N, res & 0b10000000 != 0);
                    }
                    Cpy => {
                        let val = fetch_oops!();
                        self.set_flag(Flags::C, self.y >= val);
                        self.set_flag(Flags::Z, self.y == val);
                        let (res, _) = self.y.overflowing_sub(val);
                        self.set_flag(Flags::N, res & 0b10000000 != 0);
                    }

                    Dec => {
                        let val = fetch_oops_store!(instr.mode);
                        write!(corrected_addr, val);
                        let (res, _) = val.overflowing_sub(1);
                        self.set_flag(Flags::Z, res == 0);
                        self.set_flag(Flags::N, res & 0b10000000 != 0);
                        write!(corrected_addr, res);
                    }
                    Dex => {
                        let (res, _) = self.x.overflowing_sub(1);
                        self.set_flag(Flags::Z, res == 0);
                        self.set_flag(Flags::N, res & 0b10000000 != 0);
                        self.x = res;
                    }
                    Dey => {
                        let (res, _) = self.y.overflowing_sub(1);
                        self.set_flag(Flags::Z, res == 0);
                        self.set_flag(Flags::N, res & 0b10000000 != 0);
                        self.y = res;
                    }

                    Eor => {
                        let val = fetch_oops!();
                        self.a ^= val;
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                    }

                    Ora => {
                        let val = fetch_oops!();
                        self.a |= val;
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                    }

                    Inc => {
                        let val = fetch_oops_store!(instr.mode);
                        write!(corrected_addr, val);
                        let (res, _) = val.overflowing_add(1);
                        self.set_flag(Flags::Z, res == 0);
                        self.set_flag(Flags::N, res & 0b10000000 != 0);
                        write!(corrected_addr, res);
                    }
                    Inx => {
                        let (res, _) = self.x.overflowing_add(1);
                        self.set_flag(Flags::Z, res == 0);
                        self.set_flag(Flags::N, res & 0b10000000 != 0);
                        self.x = res;
                    }
                    Iny => {
                        let (res, _) = self.y.overflowing_add(1);
                        self.set_flag(Flags::Z, res == 0);
                        self.set_flag(Flags::N, res & 0b10000000 != 0);
                        self.y = res;
                    }

                    Jmp => self.pc = fetch_addr,

                    Lda => {
                        self.a = fetch_oops!();
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                    }
                    Ldx => {
                        self.x = fetch_oops!();
                        self.set_flag(Flags::Z, self.x == 0);
                        self.set_flag(Flags::N, self.x & 0b10000000 != 0);
                    }
                    Ldy => {
                        self.y = fetch_oops!();
                        self.set_flag(Flags::Z, self.y == 0);
                        self.set_flag(Flags::N, self.y & 0b10000000 != 0);
                    }

                    Sta => match instr.mode {
                        Abx | Aby | Izy => {
                            let _ = fetch!(fetch_addr);
                            write!(corrected_addr, self.a)
                        }
                        _ => write!(fetch_addr, self.a),
                    }
                    Stx => write!(fetch_addr, self.x),
                    Sty => write!(fetch_addr, self.y),

                    Tax => {
                        self.x = self.a;
                        self.set_flag(Flags::Z, self.x == 0);
                        self.set_flag(Flags::N, self.x & 0b10000000 != 0);
                    }
                    Tay => {
                        self.y = self.a;
                        self.set_flag(Flags::Z, self.y == 0);
                        self.set_flag(Flags::N, self.y & 0b10000000 != 0);
                    }
                    Tsx => {
                        self.x = self.sp;
                        self.set_flag(Flags::Z, self.x == 0);
                        self.set_flag(Flags::N, self.x & 0b10000000 != 0);
                    }
                    Txa => {
                        self.a = self.x;
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                    }
                    Tya => {
                        self.a = self.y;
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                    }
                    Txs => {
                        self.sp = self.x;
                    }

                    Pha => push!(self.a),
                    Php => {
                        push!((self.status | Flags::U | Flags::B).bits())
                    }

                    Pla => {
                        let _ = peek!();
                        self.a = pop!();
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                    }
                    Plp =>
                    {
                        let _ = peek!();
                        let new_status = Flags::from_bits_retain(pop!());
                        self.set_flag(Flags::U, true);
                        self.set_flag(Flags::C, new_status.contains(Flags::C));
                        self.set_flag(Flags::Z, new_status.contains(Flags::Z));
                        self.set_flag(Flags::D, new_status.contains(Flags::D));
                        self.set_flag(Flags::V, new_status.contains(Flags::V));
                        self.set_flag(Flags::N, new_status.contains(Flags::N));
                        delayed_i_flag = Some(new_status.contains(Flags::I));
                    }

                    Brk => {
                        // Implied already did dummy read
                        self.pc = self.pc.wrapping_add(1);
                        self.set_flag(Flags::B, true);
                        push!(((self.pc & 0xFF00) >> 8) as u8); // Push PCH
                        push!((self.pc & 0x00FF) as u8); // Push PCL
                        self.set_flag(Flags::U, true);
                        push!(self.status.bits());
                        // self.set_flag(Flags::I, true); // ???
                        // Check for NMI hijack
                        if self.nmi_signal.load(Ordering::Relaxed) {
                            let pcl = fetch!(0xFFFA) as u16;
                            self.set_flag(Flags::I, true);
                            let pch = fetch!(0xFFFB) as u16;
                            self.pc = (pch << 8) | pcl;
                        } else {
                            let pcl = fetch!(0xFFFE) as u16;
                            self.set_flag(Flags::I, true);
                            let pch = fetch!(0xFFFF) as u16;
                            self.pc = (pch << 8) | pcl
                        }
                    }
                    Rti => {
                        let _ = peek!();
                        self.status = Flags::from_bits_retain(pop!()) | Flags::U;
                        let pcl = pop!() as u16;
                        let pch = pop!() as u16;
                        self.pc = (pch << 8) | pcl
                    }
                    Rts => {
                        let _ = peek!();
                        let pcl = pop!() as u16;
                        let pch = pop!() as u16;
                        self.pc = (pch << 8) | pcl;
                        let _ = fetch!(self.pc);
                        self.pc = self.pc.wrapping_add(1);
                    }
                    Jsr => {
                        let lo = next_pc!() as u16;
                        let _ = peek!();
                        push!(((self.pc & 0xFF00) >> 8) as u8); // Push PCH
                        push!((self.pc & 0x00FF) as u8); // Push PCL
                        let hi = next_pc!() as u16;
                        self.pc = (hi << 8) | lo;
                    }

                    // Unofficial opcodes
                    Lax => {
                        self.a = fetch_oops!();
                        self.x = self.a;
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                        // TODO: simulate instability (noise) on
                        // immediate addressing mode?
                    }
                    Sax => match instr.mode {
                        Abx | Aby | Izy => {
                            let _ = fetch!(fetch_addr);
                            write!(corrected_addr, self.a & self.x)
                        }
                        _ => write!(fetch_addr, self.a & self.x),
                    }
                    Dcp => {
                        let val = fetch_oops_store!(instr.mode);
                        write!(corrected_addr, val);
                        let (res1, _) = val.overflowing_sub(1);
                        write!(corrected_addr, res1);
                        self.set_flag(Flags::C, self.a >= res1);
                        self.set_flag(Flags::Z, self.a == res1);
                        let (res2, _) = self.a.overflowing_sub(res1);
                        self.set_flag(Flags::N, res2 & 0b10000000 != 0)
                    }
                    Isc => {
                        let val = fetch_oops_store!(instr.mode);
                        write!(corrected_addr, val);
                        let (res1, _) = val.overflowing_add(1);
                        write!(corrected_addr, res1);

                        let signed_val = res1 ^ 0xFF;
                        let res2 = signed_val as u16 + self.a as u16 +
                            self.get_flag(Flags::C) as u16;
                        let a_sign = self.a & 0b10000000;
                        let val_sign = signed_val & 0b10000000;
                        let res2_sign = res2 as u8 & 0b10000000;
                        let signed_overflow = a_sign == val_sign && a_sign != res2_sign;
                        self.a = res2 as u8;
                        self.set_flag(Flags::C, res2 & 0xFF00 != 0);
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::V, signed_overflow);
                        self.set_flag(Flags::N, res2_sign > 0);
                    }

                    Slo => {
                        match instr.mode {
                            AddrMode::Imp => {
                                let shifted_bit = self.a & 0b10000000;
                                self.a <<= 1;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                self.set_flag(Flags::Z, self.a == 0);
                                self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                            }
                            _ => {
                                let val = fetch_oops_store!(instr.mode);
                                write!(corrected_addr, val);
                                let shifted_bit = val & 0b10000000;
                                let res = val << 1;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                write!(corrected_addr, res);
                                self.a |= res;
                                self.set_flag(Flags::Z, self.a == 0);
                                self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                            }
                        }
                    }

                    Rla => {
                        match instr.mode {
                            AddrMode::Imp => {
                                let shifted_bit = self.a & 0b10000000;
                                self.a = (self.a << 1) | self.get_flag(Flags::C) as u8;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                self.set_flag(Flags::Z, self.a == 0);
                                self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                            }
                            _ => {
                                let val = fetch_oops_store!(instr.mode);
                                write!(corrected_addr, val);
                                let shifted_bit = val & 0b10000000;
                                let res = (val << 1) | self.get_flag(Flags::C) as u8;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                write!(corrected_addr, res);
                                self.a &= res;
                                self.set_flag(Flags::Z, self.a == 0);
                                self.set_flag(Flags::N, self.a & 0x80 != 0);
                            }
                        }
                    }

                    Sre => {
                        match instr.mode {
                            AddrMode::Imp => {
                                let shifted_bit = self.a & 1;
                                self.a >>= 1;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                self.a ^= self.a;
                                self.set_flag(Flags::Z, self.a == 0);
                                self.set_flag(Flags::N, false);
                            }
                            _ => {
                                let val = fetch_oops_store!(instr.mode);
                                write!(corrected_addr, val);
                                let shifted_bit = val & 1;
                                let res = val >> 1;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                write!(corrected_addr, res);
                                self.a ^= res;
                                self.set_flag(Flags::Z, self.a == 0);
                                self.set_flag(Flags::N, self.a & 0b10000000 != 0);
                            }
                        }
                    }

                    Rra => {
                        let val = match instr.mode {
                            AddrMode::Imp => {
                                let shifted_bit = self.a & 1;
                                self.a =
                                    (self.get_flag(Flags::C) as u8) << 7 | self.a >> 1;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                self.a
                            }
                            _ => {
                                let val = fetch_oops_store!(instr.mode);
                                write!(corrected_addr, val);
                                let shifted_bit = val & 1;
                                let res =
                                    (self.get_flag(Flags::C) as u8) << 7 | val >> 1;
                                self.set_flag(Flags::C, shifted_bit != 0);
                                write!(corrected_addr, res);
                                res
                            }
                        };
                        let res =
                            val as u16 + self.a as u16 + self.get_flag(Flags::C) as u16;
                        let a_sign = self.a & 0b10000000;
                        let val_sign = val & 0b10000000;
                        let res_sign = res as u8 & 0b10000000;
                        let signed_overflow = a_sign == val_sign && a_sign != res_sign;
                        self.a = res as u8;
                        self.set_flag(Flags::C, res & 0xFF00 != 0);
                        self.set_flag(Flags::Z, self.a == 0);
                        self.set_flag(Flags::V, signed_overflow);
                        self.set_flag(Flags::N, res_sign > 0)
                    }

                    // Kills the program
                    Hlt => {
                        return "HALT".into()
                    }

                    Xxx => return format!("Unsupported opcode {:02x}", instr_code)
                }
            }
        };
        Box::new(go)
    }
}

#[derive(Clone, Copy, Debug)]
enum AddrMode {
    Imp, Imm, Zp0, Zpx, Zpy, Rel, Abs, Abx, Aby, Ind, Izx, Izy
}
use AddrMode::*;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Opcode {
    // Official opcodes
    Adc, And, Asl, Bcc, Bcs, Beq, Bit, Bmi, Bne, Bpl, Brk, Bvc, Bvs,
    Clc, Cld, Cli, Clv, Cmp, Cpx, Cpy, Dec, Dex, Dey, Eor, Inc, Inx,
    Iny, Jmp, Jsr, Lda, Ldx, Ldy, Lsr, Nop, Ora, Pha, Php, Pla, Plp,
    Rol, Ror, Rti, Rts, Sbc, Sec, Sed, Sei, Sta, Stx, Sty, Tax, Tay,
    Tsx, Txa, Txs, Tya,

    // Unofficial opcodes
    Lax, Sax, Dcp, Isc, Slo, Rla, Sre, Rra, Hlt,

    // Placeholder for unsupported opcodes
    Xxx
}
use Opcode::*;

#[derive(Clone, Copy, Debug)]
struct Instr {
    opcode: Opcode,
    mode: AddrMode,
}

impl fmt::Display for Instr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?} {:?}", self.opcode, self.mode)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Arg {
    Rel(i8),
    Abs(u16),
    Imm(u8),
}

#[derive(Clone, Copy, Debug)]
pub struct InstrWithArg {
    instr: Instr,
    arg: Option<Arg>
}

impl fmt::Display for Arg {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Arg::Rel(x) => write!(f, "Rel({:x})", x),
            Arg::Abs(x) => write!(f, "Abs({:x})", x),
            Arg::Imm(x) => write!(f, "Imm({:x})", x),
        }
    }
}

impl fmt::Display for InstrWithArg {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.arg {
            None => write!(f, "{}", self.instr),
            Some(x) => write!(f, "{} {}", self.instr, x)
        }
    }
}

pub fn parse_instr_with_arg(mem: &[u8], pc: usize) -> (InstrWithArg, usize) {
    let instr_code = mem[pc];
    let instr = Cpu::decode(instr_code);
    match instr.mode {
        Imp => (InstrWithArg { instr: *instr,
                               arg: None }, 1),
        Rel => {
            let byte = mem[pc+1];
            (InstrWithArg { instr: *instr,
                            arg: Some(Arg::Rel(byte as i8)) }, 1)
        }
        Imm => {
            let byte = mem[pc+1];
            (InstrWithArg { instr: *instr,
                            arg: Some(Arg::Imm(byte)) }, 1)
        }
        _ => {
            let lo = mem[pc+1] as u16;
            let hi = mem[pc+2] as u16;
            (InstrWithArg { instr: *instr,
                            arg: Some(Arg::Abs((hi << 8) | lo)) }, 2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::ops::{CoroutineState};
    use std::pin::Pin;
    use serde::{Deserialize, Serialize};

    // #[test]
    // fn example() -> Result<(), Box<dyn std::error::Error>> {
    //     // Create and initialize cpu state
    //     let mut cpu = Cpu::new();
    //     // cpu.pc = ...

    //     // Get reference to its IO bus
    //     let cpu_bus: Arc<AtomicU8> = cpu.io_bus.clone();

    //     // Spin it up
    //     let mut cpu_process = cpu.run();
    //     Ok(())
    // }

    // { "name": "00 3f f7",
    //    "initial": { "pc": 35714,
    //                  "s": 81,
    //                  "a": 203,
    //                  "x": 117,
    //                  "y": 162,
    //                  "p": 106,
    //                  "ram": [ [35714, 0], [35715, 63], [35716, 247], [65534, 212], [65535, 37], [9684, 237]]},
    //    "final": { "pc":
    //                9684,
    //                "s":
    //                78,
    //                "a": 203,
    //                "x": 117,
    //                "y": 162, "p": 110, "ram": [ [335, 122], [336, 132], [337, 139], [9684, 237], [35714, 0], [35715, 63], [35716, 247], [65534, 212], [65535, 37]]}, "cycles": [ [35714, 0, "read"], [35715, 63, "read"], [337, 139, "write"], [336, 132, "write"], [335, 122, "write"], [65534, 212, "read"], [65535, 37, "read"]] },

    #[derive(Debug, Serialize, Deserialize)]
    struct TestState {
        pc: u16,
        s: u8,
        a: u8,
        x: u8,
        y: u8,
        p: u8,
        ram: Vec<(u16, u8)>
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Test {
        name: String,
        initial: TestState,
        r#final: TestState,
        cycles: Vec<(u16, u8, String)>,
    }

    fn run_harte_test(test: Test) -> Result<(), String> {
        // println!("{:?}", test);

        let mut cpu = Cpu::new();
        cpu.pc = test.initial.pc;
        cpu.sp = test.initial.s;
        cpu.a = test.initial.a;
        cpu.x = test.initial.x;
        cpu.y = test.initial.y;
        cpu.status = Flags::from_bits_retain(test.initial.p);
        let cpu_bus = cpu.io_bus.clone();
        let mut cpu_process = cpu.run();

        let mut mem = vec![0; 2_usize.pow(16)];
        for (addr, val) in test.initial.ram {
            mem[addr as usize] = val;
        }

        for (test_addr, test_val, test_rw) in test.cycles {
            match Pin::new(&mut cpu_process).resume(()) {
                CoroutineState::Yielded((addr, event)) => {
                    // println!("{}", event);
                    match event {
                        Some(IO::Read) => {
                            if test_rw != "read" || addr != test_addr {
                                return Err(format!("expected '{} {:04x}', got {:?} {}",
                                                   test_rw, test_addr, event, addr))
                            }
                            if mem[addr as usize] != test_val {
                                return Err(format!(
                                    "{:?} {} wrong memory value. \
                                     expected {:x}, got {:x}",
                                    event, addr, test_val, mem[addr as usize]))
                            }
                            cpu_bus.store(mem[addr as usize], Ordering::Relaxed)
                        }
                        Some(IO::Write) => {
                            if test_rw != "write" || addr != test_addr {
                                return Err(format!("expected '{} {:04x}', got {:?} {}",
                                                   test_rw, test_addr, event, addr))
                            }
                            mem[addr as usize] = cpu_bus.load(Ordering::Relaxed)
                        }
                        None => ()
                    }
                }
                CoroutineState::Complete(msg) => {
                    return Err(msg)
                }
            }
        }
        Ok(())
    }

    #[test]
    fn harte() -> Result<(), Box<dyn std::error::Error>> {
        for op in 0..=255 {
            // Ignore unsupported and HLT/STP opcodes
            if Cpu::decode(op).opcode == Opcode::Xxx ||
                Cpu::decode(op).opcode == Opcode::Hlt {
                    continue
                }
            println!("{:02x}", op);
            let path = format!("test/harte/v1/{:02x}.json", op);
            let file = fs::File::open(path)?;
            let tests: Vec<Test> = serde_json::from_reader(file)?;
            for test in tests {
                run_harte_test(test)?
            }
        }
        Ok(())
    }

    fn load_mem(path: &str, start_addr: u16)
                -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let bin = std::fs::read(path)?;
        let mut mem = [0; 65536];
        mem[start_addr as usize..start_addr as usize + bin.len()].copy_from_slice(&bin);
        Ok(mem.into())
    }

    #[test]
    fn klaus_functional() -> Result<(), Box<dyn std::error::Error>> {
        const PROGRAM_START: u16 = 0x400;
        const SUCCESS_ADDR: u16 = 0x336d;
        let path = "test/klaus/6502_functional_test.bin";
        let mut mem: Vec<u8> = load_mem(path, 0x000A)?;

        let mut cpu = Cpu::new();
        cpu.pc = PROGRAM_START;
        let cpu_bus = cpu.io_bus.clone();
        let pc_monitor = cpu.pc_monitor.clone();
        let mut cpu_process = cpu.run();

        let mut n = 0;

        let mut old_pc = 1;
        let mut same_pc_counter = 0;
        loop {
            n += 1;
            let pc = pc_monitor.load(Ordering::Relaxed);
            if pc == old_pc {
                same_pc_counter += 1;
                if same_pc_counter >= 10 {
                    break
                }
            } else {
                same_pc_counter = 0;
            }
            old_pc = pc;

            match Pin::new(&mut cpu_process).resume(()) {
                CoroutineState::Yielded((addr, event)) => {
                    match event {
                        Some(IO::Read) => {
                            cpu_bus.store(mem[addr as usize], Ordering::Relaxed)
                        }
                        Some(IO::Write) => {
                            mem[addr as usize] = cpu_bus.load(Ordering::Relaxed)
                        }
                        None => ()
                    }
                }
                CoroutineState::Complete(msg) => {
                    panic!("{}", msg)
                }
            }
        }

        if old_pc == SUCCESS_ADDR {
            Ok(())
        } else {
            println!("{}", n);
            let (instr, _) = parse_instr_with_arg(&mem, old_pc.into());
            println!("{}", instr);
            Err(format!("trapped at {:04x}", old_pc).into())
        }
    }

    #[test]
    fn klaus_interrupt() -> Result<(), Box<dyn std::error::Error>> {
        const PROGRAM_START: u16 = 0x400;
        const SUCCESS_ADDR: u16 = 0x06F5;
        const FEEDBACK_ADDR: usize = 0xBFFC;
        const IRQ_BIT: u8 = 1 << 0;
        const NMI_BIT: u8 = 1 << 1;
        let path = "test/klaus/6502_interrupt_test.bin";
        let mut mem: Vec<u8> = load_mem(path, 0x000A)?;

        let mut cpu = Cpu::new();
        cpu.pc = PROGRAM_START;
        let cpu_bus = cpu.io_bus.clone();
        let pc_monitor = cpu.pc_monitor.clone();
        let irq_signal = cpu.irq_signal.clone();
        let nmi_signal = cpu.nmi_signal.clone();
        let mut cpu_process = cpu.run();

        let mut n = 0;

        let mut old_pc = 1;
        let mut same_pc_counter = 0;
        let mut prev_irq = mem[FEEDBACK_ADDR] & IRQ_BIT != 0;
        let mut prev_nmi = mem[FEEDBACK_ADDR] & IRQ_BIT != 0;
        loop {
            n += 1;

            let irq = mem[FEEDBACK_ADDR] & IRQ_BIT != 0;
            let nmi = mem[FEEDBACK_ADDR] & NMI_BIT != 0;
            if nmi && !prev_nmi {
                println!("triggering nmi");
                nmi_signal.store(true, Ordering::Relaxed);
            }
            if irq && !prev_irq {
                println!("triggering irq");
                irq_signal.store(true, Ordering::Relaxed);
            }
            prev_irq = irq;
            prev_nmi = nmi;

            let pc = pc_monitor.load(Ordering::Relaxed);
            if pc == old_pc {
                same_pc_counter += 1;
                if same_pc_counter >= 10 {
                    break
                }
            } else {
                same_pc_counter = 0;
            }
            old_pc = pc;

            match Pin::new(&mut cpu_process).resume(()) {
                CoroutineState::Yielded((addr, event)) => {
                    match event {
                        Some(IO::Read) => {
                            cpu_bus.store(mem[addr as usize], Ordering::Relaxed)
                        }
                        Some(IO::Write) => {
                            mem[addr as usize] = cpu_bus.load(Ordering::Relaxed)
                        }
                        None => ()
                    }
                }
                CoroutineState::Complete(msg) => {
                    panic!("{}", msg)
                }
            }
        }

        println!("{}", n);
        if old_pc == SUCCESS_ADDR {
            Ok(())
        } else {
            let (instr, _) = parse_instr_with_arg(&mem, old_pc.into());
            println!("{}", instr);
            Err(format!("trapped at {:04x}", old_pc).into())
        }
    }
}
